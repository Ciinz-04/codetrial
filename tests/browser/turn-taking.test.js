import { test } from "node:test";
import assert from "node:assert/strict";
import { functionBody, read } from "./source.js";
import {
  TURN_RING_LINGER_MS,
  TURN_SPEECH_PEAK,
  TURN_WINDOW_ATTRIBUTE,
  isYieldShortcut,
  turnCountdown,
  turnWindowMs,
  yieldTurnPayload,
} from "../../web/lib.js";

/// Loads named functions out of web/interview.js against `scope`, which stands
/// in for the module around them. Names not in `scope` fall through to the
/// real globals, which is how `JSON` and `TextDecoder` reach them.
function loadInterview(names, scope) {
  const source = read("web/interview.js");
  const bodies = names.map((name) => `${functionBody(source, name)}\n}`);
  const exports = `{ ${names.join(", ")} }`;
  return new Function(
    "scope",
    `with (scope) { ${bodies.join("\n")}\n return ${exports}; }`,
  )(new Proxy(scope, { has: (target, key) => key in target }));
}

test("the turn controls publish only in a live, connected, unpaused interview", () => {
  const sent = [];
  const state = { connected: true, paused: false, phase: "live" };
  const scope = {
    state,
    nodes: { editor: {} },
    topics: { control: "control" },
    publish: (topic, payload) => {
      sent.push([topic, payload]);
      return Promise.resolve();
    },
    yieldTurnPayload,
    isYieldShortcut,
  };
  const { yieldTurn, onTurnKey } = loadInterview(
    ["canTakeTurnAction", "yieldTurn", "onTurnKey"],
    scope,
  );
  yieldTurn();
  assert.deepEqual(sent, [["control", { type: "yield_turn" }]]);

  let prevented = 0;
  const key = (target) => ({
    altKey: true,
    key: "Enter",
    target,
    preventDefault: () => {
      prevented += 1;
    },
  });
  onTurnKey(key({ tagName: "DIV" }));
  assert.equal(prevented, 1);
  assert.equal(sent.length, 2);

  for (const blocked of [
    () => (state.paused = true),
    () => (state.connected = false),
    () => (state.phase = "ending"),
  ]) {
    Object.assign(state, { connected: true, paused: false, phase: "live" });
    blocked();
    yieldTurn();
    onTurnKey(key({ tagName: "DIV" }));
  }
  assert.equal(sent.length, 2, "nothing published outside a live interview");
  assert.equal(prevented, 1, "the key is left alone when it does nothing");
});

test("Alt+Enter yields from the page and the code editor, never from another field", () => {
  const editor = { tagName: "TEXTAREA" };
  const press = (target, extra = {}) =>
    isYieldShortcut({ altKey: true, key: "Enter", target, ...extra }, editor);
  assert.equal(press({ tagName: "DIV" }), true);
  assert.equal(press(editor), true);
  assert.equal(press({ tagName: "INPUT" }), false);
  assert.equal(press({ tagName: "TEXTAREA" }), false);
  assert.equal(press({ tagName: "SELECT" }), false);
  assert.equal(press({ tagName: "DIV", isContentEditable: true }), false);
  assert.equal(press({ tagName: "DIV" }, { repeat: true }), false);
  assert.equal(press({ tagName: "DIV" }, { isComposing: true }), false);
  assert.equal(press({ tagName: "DIV" }, { defaultPrevented: true }), false);
  assert.equal(press({ tagName: "DIV" }, { shiftKey: true }), false);
  assert.equal(press({ tagName: "DIV" }, { ctrlKey: true }), false);
  assert.equal(press({ tagName: "DIV" }, { altKey: false }), false);
  assert.equal(press({ tagName: "DIV" }, { key: "a" }), false);
});

test("the ring fills through the published silence window and resets on speech", () => {
  const silenceMs = 3000;
  const speech = TURN_SPEECH_PEAK;
  const quiet = TURN_SPEECH_PEAK / 2;
  let spokeAt = null;
  const frame = (peak, at, blocked = false) => {
    const next = turnCountdown(spokeAt, { peak, at, silenceMs, blocked });
    spokeAt = next.spokeAt;
    return next.progress;
  };
  assert.equal(frame(quiet, 0), null, "nothing to count before speech");
  assert.equal(frame(speech, 100), 0);
  assert.equal(frame(quiet, 100 + 1500), 0.5);
  assert.equal(frame(speech, 2000), 0, "speaking again resets it");
  assert.equal(frame(quiet, 2000 + 3000), 1);
  assert.equal(frame(quiet, 2000 + 3000 + TURN_RING_LINGER_MS), 1);
  assert.equal(
    frame(quiet, 2000 + 3000 + TURN_RING_LINGER_MS + 1),
    null,
    "a full ring Jim never answered comes down",
  );
  assert.equal(frame(speech, 9000), 0);
  assert.equal(frame(quiet, 9100, true), null, "Jim speaking clears it");
  assert.equal(frame(quiet, 9200), null, "and it waits for new speech");
  assert.deepEqual(
    turnCountdown(null, { peak: speech, at: 0, silenceMs: null }),
    { spokeAt: null, progress: null },
    "no ring without a published window",
  );
});

test("the silence window is read from the agent's attribute and never guessed", () => {
  assert.equal(TURN_WINDOW_ATTRIBUTE, "codetrial.silence_ms");
  assert.equal(turnWindowMs({ [TURN_WINDOW_ATTRIBUTE]: "3000" }), 3000);
  assert.equal(turnWindowMs({ [TURN_WINDOW_ATTRIBUTE]: "0" }), null);
  assert.equal(turnWindowMs({ [TURN_WINDOW_ATTRIBUTE]: "soon" }), null);
  assert.equal(turnWindowMs({}), null);
  assert.equal(turnWindowMs(undefined), null);
});

test("the ring meters one microphone at a time and stops with its track", () => {
  const meters = [];
  const turnRing = { hidden: false, style: { setProperty() {} } };
  const scope = {
    state: { phase: "live" },
    nodes: { turnRing },
    turnSpokeAt: null,
    turnRingProgress: null,
    turnMeter: null,
    turnMeterTrack: null,
    paintTurnRing: () => {},
    startMediaMeter: () => {},
    createMicMeter: (options) => {
      const meter = {
        options,
        started: 0,
        forgotten: 0,
        start: () => (meter.started += 1),
        forget: () => (meter.forgotten += 1),
      };
      meters.push(meter);
      return meter;
    },
  };
  const { startTurnRing } = loadInterview(
    ["hideTurnRing", "stopTurnRing", "startTurnRing"],
    scope,
  );
  const track = () => {
    const listeners = {};
    return {
      readyState: "live",
      addEventListener: (name, listener) => (listeners[name] = listener),
      end: () => listeners.ended?.(),
    };
  };
  const stream = (audio) => ({ getAudioTracks: () => [audio] });

  const first = track();
  startTurnRing(stream(first));
  assert.equal(meters.length, 1);
  assert.equal(meters[0].started, 1);
  assert.equal(meters[0].options.onLevel, scope.paintTurnRing);
  startTurnRing(stream(first));
  assert.equal(meters.length, 1, "the same track keeps its meter");

  const second = track();
  startTurnRing(stream(second));
  assert.equal(meters[0].forgotten, 1, "a new track retires the old meter");
  assert.equal(meters[1].started, 1);
  first.end();
  assert.equal(meters[1].forgotten, 0, "an old track ending is ignored");
  turnRing.hidden = false;
  second.end();
  assert.equal(meters[1].forgotten, 1, "its own track ending retires it");
  assert.equal(turnRing.hidden, true);
  scope.state.phase = "ending";
  assert.equal(meters[1].options.isFinished(), true);

  startTurnRing(stream({ readyState: "ended" }));
  startTurnRing(undefined);
  assert.equal(meters.length, 2, "no meter without a live track");
});
