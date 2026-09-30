//! The `tests` module of `src/livekit/turn.rs`, which declares this file by
//! path.
//! Everything here reaches into `src/livekit/turn.rs` through `super`, so it is
//! a unit
//! test and not an integration test: private items are in scope.

use super::*;
use crate::agent::ThinkingHold;

/// A pause arms the discard so a turn already in flight cannot speak over
/// the resume. Arming it on a turn that has finished is the failure: its
/// completion event has already passed, so nothing disarms it and the first
/// reply after the resume is swallowed instead, which is silence exactly
/// where the candidate is waiting to be answered.
#[test]
fn only_a_turn_still_being_produced_leaves_output_to_discard() {
    assert!(pause_leaves_output_in_flight(Floor::Speaking));
    assert!(
        !pause_leaves_output_in_flight(Floor::AwaitingPlayout),
        "the turn is complete and draining, so no event is coming to disarm this"
    );
    assert!(!pause_leaves_output_in_flight(Floor::Listening));
}

#[test]
fn candidate_exit_skips_wrap_up_before_report() {
    assert!(!should_send_wrap_up("candidate_ended"));
    assert!(should_send_wrap_up("time_up"));

    // Jim is told not to say goodbye before calling the tool, so the wrap-up is
    // the only thing that speaks the closing on its route out.
    assert!(should_send_wrap_up("interview_complete"));
}

/// An idle-window review runs in a pause and only in a pause.
///
/// Every condition is a way the room is busy, and each costs something
/// different if it is dropped: reviewing while Jim holds the floor spends a
/// call on a stretch still being said, reviewing with a tool response owed
/// races the turn it belongs to, and reviewing a stretch with nothing new in it
/// pays for a second reading of the same silence.
#[test]
fn a_pause_is_read_only_when_the_room_is_actually_idle() {
    let start = Instant::now();
    let idle = start + INTERIM_COOLDOWN + INTERIM_IDLE;
    let quiet = || RuntimeState {
        transcript: (0..INTERIM_MIN_NEW_TURNS)
            .map(|index| format!("Candidate: line {index}"))
            .collect(),
        ..RuntimeState::default()
    };
    let mut activity = RuntimeActivity::new(start);
    assert!(activity.interim_review_due(&quiet(), idle));

    // Each case is the idle room with one thing wrong with it.
    /// One way the room is busy: the thing to break, and why it disqualifies
    /// the pause.
    type Busy = (&'static str, fn(&mut RuntimeActivity, &mut RuntimeState));
    let busy: [Busy; 8] = [
        (
            "Jim is mid-sentence, so the stretch is not finished",
            |activity, _| {
                activity.mark_speaking();
            },
        ),
        (
            "a generation is already owed on this socket",
            |activity, _| {
                activity.tool_response_outstanding = true;
            },
        ),
        (
            "the candidate is waiting on a reply, so the pause is Jim's",
            |activity, _| {
                activity.awaiting_reply_since = Some(Instant::now());
            },
        ),
        // Expressed as "spoke a second ago", not as an instant in the future: a
        // stamp ahead of `now` fails this too, but only because
        // `duration_since` saturates to zero, which is not the rule being
        // pinned.
        (
            "the candidate has only just stopped talking",
            |activity, _| {
                activity.last_user_speech +=
                    INTERIM_COOLDOWN + INTERIM_IDLE - Duration::from_secs(1);
            },
        ),
        (
            "a paused interview is not idle, it is stopped",
            |_, state| {
                state.paused = true;
            },
        ),
        ("a stretch already read is not read again", |_, state| {
            state.interim_transcript_lines = state.transcript.len();
        }),
        (
            "the interviewer asked to close, and the end aborts a running review",
            |_, state| {
                state.end_requested = true;
            },
        ),
        (
            "the planned time runs out before the call could return",
            |_, state| {
                let planned = u64::from(state.coding_minutes + state.behavioral_minutes) * 60;
                state.started_at -= Duration::from_secs(planned - 5);
            },
        ),
    ];
    for (why, break_it) in busy {
        let mut activity = RuntimeActivity::new(start);
        let mut state = quiet();
        break_it(&mut activity, &mut state);
        assert!(!activity.interim_review_due(&state, idle), "{why}");
    }

    assert!(
        !activity.interim_review_due(&quiet(), start + INTERIM_IDLE),
        "two reviews in one interview are not two reviews in one minute"
    );
    assert!(
        !activity.interim_review_due(
            &RuntimeState {
                transcript: (0..INTERIM_MIN_NEW_TURNS * 2)
                    .map(|index| format!("Interviewer: line {index}"))
                    .collect(),
                ..RuntimeState::default()
            },
            idle
        ),
        "the interviewer talking to itself is not new evidence about the candidate"
    );

    // Claiming the pause is what spends it: the cooldown is stamped where it is
    // read, so the next tick cannot start a second review.
    assert!(activity.claim_interim_review(&quiet(), idle));
    assert!(!activity.claim_interim_review(&quiet(), idle));
}

#[test]
fn interim_review_stops_at_the_cap() {
    let start = Instant::now();
    let state = RuntimeState {
        transcript: (0..INTERIM_MIN_NEW_TURNS)
            .map(|index| format!("Candidate: line {index}"))
            .collect(),
        ..RuntimeState::default()
    };
    let mut activity = RuntimeActivity::with_interim_review_cap(start, 2);
    let first = start + INTERIM_COOLDOWN + INTERIM_IDLE;

    assert!(activity.claim_interim_review(&state, first));
    assert!(activity.claim_interim_review(&state, first + INTERIM_COOLDOWN));
    assert!(!activity.claim_interim_review(&state, first + INTERIM_COOLDOWN * 2));
}

#[test]
fn interim_review_cap_of_zero_claims_none() {
    let start = Instant::now();
    let state = RuntimeState {
        transcript: (0..INTERIM_MIN_NEW_TURNS)
            .map(|index| format!("Candidate: line {index}"))
            .collect(),
        ..RuntimeState::default()
    };
    let mut activity = RuntimeActivity::with_interim_review_cap(start, 0);

    assert!(!activity.claim_interim_review(&state, start + INTERIM_COOLDOWN + INTERIM_IDLE));
}

/// The idle-window review's constants are eleven numbers in three modules, and
/// three pairs of them are load-bearing on each other. Written down here in the
/// shape `report_network_budget_covers_every_repair_and_retry_per_generation`
/// established: the arithmetic a design depends on is asserted, not described,
/// because a comment saying two numbers must relate is checked by nobody.
///
/// One relationship is missing from this list on purpose. The reserve being
/// smaller than the store is asserted at the declaration, where whoever changes
/// either number is already standing; restating it here would be a second copy
/// of the fact, which is what this test exists to prevent.
#[test]
fn one_review_at_a_time_is_arithmetic_and_not_a_hope() {
    use crate::agent::{INTERIM_CONTEXT_NOTES, MAX_INTERIM_LINES_PER_REVIEW, MAX_INTERIM_NOTES};
    use crate::config::DEFAULT_MAX_INTERIM_REVIEWS;
    use crate::gemini::INTERIM_ATTEMPT_TIMEOUT;

    // A call cannot outlive the wait for the next chance to start one. This is
    // what makes the slot in `InterimReview` unable to be occupied when a pause
    // comes due, so the two mechanisms cannot disagree about whether a review
    // is running.
    const { assert!(INTERIM_ATTEMPT_TIMEOUT.as_secs() < INTERIM_COOLDOWN.as_secs()) };

    // What a later review is shown has to leave room for what it may add, or
    // every review is handed a context it cannot help repeating.
    const { assert!(INTERIM_CONTEXT_NOTES + MAX_INTERIM_LINES_PER_REVIEW < MAX_INTERIM_NOTES) };

    // The default quota fits the retained note budget, so no review evicts a
    // previous one before the interview ends. It uses half: the budget is sized
    // for an operator who sets twelve.
    const { assert!(DEFAULT_MAX_INTERIM_REVIEWS * MAX_INTERIM_LINES_PER_REVIEW <= MAX_INTERIM_NOTES) };

    // A pause has to be long enough to be worth reading and short enough to
    // happen; a threshold at or above the cooldown would mean the cooldown
    // never decided anything.
    const { assert!(INTERIM_IDLE.as_secs() < INTERIM_COOLDOWN.as_secs()) };

    // What one call is asked to read, against the seconds it has to read it.
    // Not a throughput claim -- it is the ceiling that keeps the deadline
    // meaningful rather than a coin flip on a long backlog. A review that
    // cannot finish inside INTERIM_ATTEMPT_TIMEOUT spends its prefill and
    // returns nothing, and the window is marked read either way.
    const { assert!(INTERIM_WINDOW_BYTES + INTERIM_CODE_BYTES <= 16 * 1024) };

    // Floored as well as capped. A budget of a couple of kilobytes reads a
    // stretch of interview as a fragment, and a review of a fragment is a note
    // about nothing -- the failure a ceiling on its own cannot see.
    const { assert!(INTERIM_WINDOW_BYTES >= 4 * 1024) };
    const { assert!(INTERIM_CODE_BYTES >= 2 * 1024) };
}

/// A line the model was shown and that has since gone is retracted in so many
/// words; left out, the model keeps holding the stale one.
#[test]
fn an_evidence_line_that_goes_away_is_retracted() {
    let lines = |items: &[&str]| {
        items
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
    };
    let shown = lines(&["tests: not run", "session: paused"]);
    assert_eq!(
        evidence_delta(Some(&shown), &lines(&["tests: not run"])),
        "session: none"
    );
    assert_eq!(
        evidence_delta(
            Some(&shown),
            &lines(&["tests: 1 of 2 passing", "session: paused"])
        ),
        "tests: 1 of 2 passing"
    );
    assert_eq!(evidence_delta(Some(&shown), &shown), "");
    assert_eq!(
        evidence_delta(None, &lines(&["tests: not run"])),
        "tests: not run"
    );
}

/// A review has to be able to return before the planned end: one whose call
/// would run into it is not started, and one a second earlier is.
#[test]
fn no_interim_review_starts_that_the_end_would_abort() {
    let state = RuntimeState {
        transcript: (0..INTERIM_MIN_NEW_TURNS)
            .map(|index| format!("Candidate: line {index}"))
            .collect(),
        ..RuntimeState::default()
    };
    let planned =
        Duration::from_secs(u64::from(state.coding_minutes + state.behavioral_minutes) * 60);
    let last_call = state.started_at + planned - crate::gemini::INTERIM_ATTEMPT_TIMEOUT;
    let activity = RuntimeActivity::new(state.started_at);
    assert!(!activity.interim_review_due(&state, last_call));
    assert!(activity.interim_review_due(&state, last_call - Duration::from_secs(1)));
}

#[test]
fn a_tool_continuation_stays_owed_after_prompt_output() {
    let start = Instant::now();
    let mut activity = RuntimeActivity::new(start);
    activity.mark_prompted(start, None, false);
    activity.note_output();
    activity.tool_response_outstanding = true;

    assert!(activity.prompted_at.is_none());
    assert!(activity.awaiting_reply_since.is_none());
    assert!(activity.owes_reply());

    activity.tool_response_outstanding = false;
    activity.mark_listening();
    assert!(!activity.owes_reply());
}

/// A prompt Gemini never answers must not hold the floor forever: a held floor
/// silences every nudge and keeps a `GoAway` waiting until the server drops the
/// socket from an older checkpoint. Releasing it keeps the reply owed, so the
/// replacement socket still answers.
#[test]
fn a_prompt_with_no_output_returns_the_floor_but_stays_owed() {
    let quiet = Stalls {
        prompt_released: false,
        spend_restart: false,
    };
    let start = Instant::now();
    let mut activity = RuntimeActivity::new(start);
    assert!(!activity.owes_reply());

    activity.mark_prompted(start, None, false);
    assert!(activity.owes_reply());
    assert_eq!(
        activity.settle_stalls(start + PROMPT_STALL - Duration::from_millis(1), false),
        quiet
    );
    assert_eq!(activity.floor, Floor::Speaking);

    assert_eq!(
        activity.settle_stalls(start + PROMPT_STALL, false),
        Stalls {
            prompt_released: true,
            spend_restart: true,
        }
    );
    assert_eq!(activity.floor, Floor::Listening);
    assert!(activity.owes_reply(), "the reply was never given");
    assert_eq!(
        activity.settle_stalls(start + PROMPT_STALL * 2, false),
        quiet,
        "a floor already returned is not returned again"
    );

    activity.mark_prompted(start, None, false);
    activity.note_output();
    assert!(!activity.owes_reply());
    assert_eq!(
        activity.settle_stalls(start + PROMPT_STALL, false),
        quiet,
        "a prompt that produced output is being answered, however slowly it plays"
    );
}

/// The candidate's own turn stalls too: they finish, Gemini sends nothing, and
/// a held `GoAway` has no later event to be spent on. The floor is already
/// theirs, so only the restart is released.
#[test]
fn an_unanswered_candidate_turn_lets_a_held_restart_go() {
    let start = Instant::now();
    let mut activity = RuntimeActivity::new(start);
    activity.note_candidate_finished(start);
    assert!(
        !activity
            .settle_stalls(start + PROMPT_STALL - Duration::from_millis(1), false)
            .spend_restart
    );
    assert_eq!(
        activity.settle_stalls(start + PROMPT_STALL, false),
        Stalls {
            prompt_released: false,
            spend_restart: true,
        }
    );
    assert!(activity.owes_reply(), "their turn is still unanswered");
}

/// A candidate who speaks after a prompt went unanswered has moved on: their
/// turn is what is owed, not the stale prompt.
#[test]
fn speaking_again_retires_an_unanswered_prompt() {
    let start = Instant::now();
    let mut activity = RuntimeActivity::new(start);
    activity.mark_prompted(start, None, false);
    activity.settle_stalls(start + PROMPT_STALL, false);
    activity.note_candidate_finished(start + PROMPT_STALL * 2);
    assert!(activity.prompted_at.is_none());
    assert!(activity.reply_in_flight());

    // A prompt that has produced nothing yet is not a reply under way, so the
    // candidate speaking over it is their turn to answer.
    let mut waiting = RuntimeActivity::new(start);
    waiting.mark_prompted(start, None, false);
    waiting.note_candidate_finished(start + Duration::from_secs(1));
    assert!(waiting.prompted_at.is_none());
    assert!(waiting.reply_in_flight());

    // While Gemini is producing, a lagging transcript fragment retires nothing:
    // the prompt went out behind that turn, and its answer may still be on its
    // way.
    let mut speaking = RuntimeActivity::new(start);
    speaking.note_output();
    speaking.mark_prompted(start, None, false);
    speaking.note_candidate_finished(start + Duration::from_secs(1));
    assert!(speaking.prompted_at.is_some());
    assert!(!speaking.reply_in_flight());
}

/// Output that follows a prompt sent mid-generation is the earlier
/// generation's tail until that turn ends, so it cannot settle the prompt.
#[test]
fn output_behind_a_prompt_belongs_to_the_earlier_turn() {
    let start = Instant::now();
    let mut activity = RuntimeActivity::new(start);
    activity.note_output();
    activity.mark_prompted(start, None, false);
    activity.note_output();
    assert!(
        activity.owes_reply(),
        "the old sentence's tail answers nothing"
    );
    activity.note_turn_boundary();
    assert!(
        activity.owes_reply(),
        "the old turn ending is not the answer"
    );
    activity.note_output();
    assert!(!activity.owes_reply());
}

/// A prompt's own turn ending with nothing said is Gemini answering with
/// silence, and replaying it later would ask what it chose not to.
#[test]
fn a_prompt_answered_with_silence_is_not_owed() {
    let start = Instant::now();
    let mut activity = RuntimeActivity::new(start);
    activity.mark_prompted(start, None, false);
    activity.note_turn_boundary();
    assert!(!activity.owes_reply());
}

/// A briefing that never reached the socket is still owed, without taking
/// the floor from a watcher that has nothing generating to wait for.
#[test]
fn an_owed_briefing_leaves_the_floor_alone() {
    let start = Instant::now();
    let mut activity = RuntimeActivity::new(start);
    activity.owe_prompt(start, None);
    assert!(activity.owes_reply());
    assert_eq!(activity.floor, Floor::Listening);
}

#[test]
fn thinking_suppresses_silence_and_editor_prompts_until_released() {
    let start = Instant::now();
    let now = start + Duration::from_secs(300);
    let mut state = RuntimeState {
        thinking_hold: ThinkingHold::Held {
            since: std::time::Instant::now(),
        },
        ..RuntimeState::default()
    };
    let mut activity = RuntimeActivity::new(start);
    assert!(activity.watch_prompt(&mut state, now).is_none());
    state.thinking_hold = ThinkingHold::Off;
    assert!(activity.watch_prompt(&mut state, now).is_some());
}

#[test]
fn tentative_spoken_holds_leave_no_declared_gaps_or_reply_over_continued_speech() {
    let mut state = RuntimeState::default();
    let mut activity = RuntimeActivity::new(Instant::now());
    activity.observe_thinking_fragment(&mut state, "Let me think", Instant::now(), 100);
    assert_eq!(state.take_thinking_notice(), None);
    assert!(state.thinking_hold.is_active());
    assert_eq!(state.evidence_ledger.lifecycle.transitions, 0);
    activity.discarding_output = true;
    activity.observe_thinking_fragment(
        &mut state,
        "Let me think about the edge cases",
        Instant::now(),
        101,
    );
    assert_eq!(state.take_thinking_notice(), None);
    assert!(!state.thinking_hold.is_active());
    assert_eq!(state.evidence_ledger.lifecycle.transitions, 0);
    // Gemini may already be answering, into a discard that drops the answer.
    assert!(activity.reply_after_thinking_discard);
    assert!(
        !activity.claim_thinking_reply(&state),
        "not while discarding"
    );

    // Cut off by the candidate still talking: their speech gets its own reply.
    activity.end_discard(true);
    assert!(!activity.claim_thinking_reply(&state));
}

#[test]
fn an_answer_dropped_under_a_hold_that_speech_ended_is_asked_for_again() {
    let mut state = RuntimeState {
        thinking_hold: ThinkingHold::Held {
            since: Instant::now(),
        },
        ..RuntimeState::default()
    };
    let mut activity = RuntimeActivity::new(Instant::now());
    activity.discarding_output = true;
    activity.observe_thinking_fragment(&mut state, "I'd use two pointers", Instant::now(), 1);
    assert!(!state.thinking_hold.is_active());

    // The dropped reply's own turn finishes: nothing of it was heard.
    activity.end_discard(false);
    assert!(activity.claim_thinking_reply(&state));
    assert!(!activity.claim_thinking_reply(&state), "asked for once");
}

#[test]
fn only_a_confirmed_request_declares_a_gap_and_ended_sessions_cannot_hold() {
    let mut state = RuntimeState::default();
    let mut activity = RuntimeActivity::new(Instant::now());
    activity.observe_thinking_fragment(
        &mut state,
        "Okay, let me think for a moment",
        Instant::now(),
        100,
    );
    assert_eq!(state.evidence_ledger.lifecycle.transitions, 0);
    activity.confirm_thinking_request(&mut state, Instant::now(), 101);
    assert_eq!(state.take_thinking_notice(), Some(true));
    assert_eq!(state.evidence_ledger.lifecycle.transitions, 1);
    activity.confirm_thinking_request(&mut state, Instant::now(), 102);
    assert_eq!(state.take_thinking_notice(), None);
    for filler in ["okay", "hmm", "so", "well", "right"] {
        activity.observe_thinking_fragment(&mut state, filler, Instant::now(), 103);
        assert_eq!(state.take_thinking_notice(), None);
        assert!(state.thinking_hold.is_active());
    }
    activity.observe_thinking_fragment(&mut state, "Ready", Instant::now(), 104);
    assert_eq!(state.take_thinking_notice(), Some(false));
    assert_eq!(state.evidence_ledger.lifecycle.transitions, 2);
    state.ended = true;
    activity.observe_thinking_fragment(&mut state, "Wait", Instant::now(), 105);
    assert_eq!(state.take_thinking_notice(), None);
    assert!(!state.thinking_hold.is_active());
    assert_eq!(state.evidence_ledger.lifecycle.transitions, 2);
}

#[test]
fn browser_actions_settle_tentative_requests_without_unpaired_hold_rows() {
    for (payload, confirmed, changed) in [
        (
            serde_json::json!({"type":"thinking","thinking":true}),
            true,
            Some(true),
        ),
        // Continue before the request was confirmed: nothing started, so
        // nothing may end in the ledger.
        (
            serde_json::json!({"type":"thinking","thinking":false}),
            false,
            Some(false),
        ),
        (serde_json::json!({"type":"yield_turn"}), false, None),
        (
            serde_json::json!({"type":"end_interview","reason":"time_expired"}),
            false,
            None,
        ),
    ] {
        let mut state = RuntimeState::default();
        let mut activity = RuntimeActivity::new(Instant::now());
        activity.observe_thinking_fragment(&mut state, "Wait", Instant::now(), 100);
        assert!(state.thinking_hold.is_requested());
        let applied = apply_control(&mut activity, &mut state, &payload);
        assert_eq!(applied.thinking_changed, changed, "{payload}");
        assert_eq!(state.thinking_hold.is_declared(), confirmed, "{payload}");
        assert!(confirmed || !state.thinking_hold.is_active(), "{payload}");
        let ended = u32::from(payload["type"] == "end_interview");
        assert_eq!(
            state.evidence_ledger.lifecycle.transitions,
            u32::from(confirmed) + ended,
            "{payload}"
        );
        if confirmed {
            apply_control(
                &mut activity,
                &mut state,
                &serde_json::json!({"type":"thinking","thinking":false}),
            );
            assert_eq!(state.evidence_ledger.lifecycle.transitions, 2);
        }
    }
}

#[test]
fn abandoning_a_request_mid_discard_owes_the_dropped_reply_and_confirming_does_not() {
    for (payload, owed) in [
        (serde_json::json!({"type":"yield_turn"}), true),
        (
            serde_json::json!({"type":"thinking","thinking":true}),
            false,
        ),
    ] {
        let mut state = RuntimeState {
            thinking_hold: ThinkingHold::Requested {
                since: std::time::Instant::now(),
            },
            ..RuntimeState::default()
        };
        let mut activity = RuntimeActivity::new(Instant::now());
        activity.discarding_output = true;
        activity.reply_after_thinking_discard = !owed;
        apply_control(&mut activity, &mut state, &payload);
        assert_eq!(activity.reply_after_thinking_discard, owed, "{payload}");
    }
}

#[test]
fn a_timed_event_abandons_an_unconfirmed_hold_and_is_delivered_at_once() {
    let mut state = RuntimeState::default();
    state.started_at -=
        Duration::from_secs(u64::from(state.coding_minutes + state.behavioral_minutes) * 60 - 200);
    let mut activity = RuntimeActivity::new(Instant::now());
    activity.observe_thinking_fragment(&mut state, "Wait", Instant::now(), 100);
    assert!(state.thinking_hold.is_requested());
    let payload = serde_json::json!({"type":"time_warning"});
    let result = apply_control(&mut activity, &mut state, &payload);
    assert!(!state.thinking_hold.is_active());
    assert!(
        result
            .generate_reply
            .unwrap()
            .contains("five-minute warning")
    );
    assert_eq!(result.thinking_changed, None);
    // A tentative hold never started in the ledger, so nothing ends there.
    assert_eq!(state.evidence_ledger.lifecycle.transitions, 0);
}

#[test]
fn a_timed_packet_the_reducer_rejects_leaves_a_request_standing() {
    // Minutes from the end, so the warning is refused as early.
    let mut state = RuntimeState::default();
    let mut activity = RuntimeActivity::new(Instant::now());
    activity.observe_thinking_fragment(&mut state, "Wait", Instant::now(), 100);
    for payload in [
        serde_json::json!({"type":"time_warning"}),
        serde_json::json!({"type":"round_transition","round":"behavioral"}),
    ] {
        let result = apply_control(&mut activity, &mut state, &payload);
        assert!(result.generate_reply.is_none(), "{payload}");
        assert!(state.thinking_hold.is_requested(), "{payload}");
    }
}

#[test]
fn a_compound_request_confirms_one_hold_and_supersedes_tentative_reply_debt() {
    let mut state = RuntimeState::default();
    let mut activity = RuntimeActivity::new(Instant::now());
    for text in ["Wait", "Wait, let me", "Wait, let me think"] {
        activity.observe_thinking_fragment(&mut state, text, Instant::now(), 100);
    }
    assert!(state.thinking_hold.is_active());
    activity.reply_after_thinking_discard = true;
    activity.confirm_thinking_request(&mut state, Instant::now(), 101);
    assert_eq!(state.take_thinking_notice(), Some(true));
    assert!(!activity.reply_after_thinking_discard);
    assert_eq!(state.evidence_ledger.lifecycle.transitions, 1);
}

#[test]
fn a_button_hold_ignores_transcripts_of_speech_from_before_the_click() {
    let click = Instant::now();
    let mut state = RuntimeState::default();
    let mut activity = RuntimeActivity::new(click);
    apply_control(
        &mut activity,
        &mut state,
        &serde_json::json!({"type":"thinking","thinking":true}),
    );
    assert_eq!(state.take_thinking_notice(), Some(true));
    let grace = thinking_transcript_grace(3_000);
    activity.ignore_input_before_hold(click, grace);

    // The transcript of what was said before the click arrives after it.
    let late = click + Duration::from_millis(300);
    activity.observe_thinking_fragment(
        &mut state,
        "I'm not sure how to handle duplicates",
        late,
        400,
    );
    assert_eq!(state.take_thinking_notice(), None);
    assert!(state.thinking_hold.is_declared());
    activity.observe_thinking_fragment(&mut state, "hmm", click + grace, 401);
    assert_eq!(state.take_thinking_notice(), None);
    assert!(
        state.thinking_hold.is_declared(),
        "a filler never ends the hold"
    );
    activity.observe_thinking_fragment(&mut state, "Ready", click + grace, 402);
    assert_eq!(state.take_thinking_notice(), Some(false));
    assert_eq!(state.evidence_ledger.lifecycle.transitions, 2);
}

#[test]
fn an_answer_given_straight_after_the_click_is_not_swallowed() {
    let click = Instant::now();
    let mut state = RuntimeState {
        thinking_hold: ThinkingHold::Held {
            since: std::time::Instant::now(),
        },
        ..RuntimeState::default()
    };
    let mut activity = RuntimeActivity::new(click);
    let grace = thinking_transcript_grace(3_000);
    activity.ignore_input_before_hold(click, grace);

    // The pre-click transcript inside the window does not stretch it: the
    // answer that follows is transcribed a silence window after it ends, so it
    // lands after the window and releases the hold.
    let before = click + Duration::from_millis(300);
    activity.observe_thinking_fragment(&mut state, "I would sort", before, 1);
    assert_eq!(state.take_thinking_notice(), None);
    let answer = click + Duration::from_millis(3_300);
    activity.observe_thinking_fragment(&mut state, "Actually it is linear", answer, 2);
    assert_eq!(state.take_thinking_notice(), Some(false));
}

#[test]
fn the_pre_click_window_stays_inside_the_silence_window() {
    assert_eq!(thinking_transcript_grace(3_000), THINKING_TRANSCRIPT_GRACE);
    assert_eq!(thinking_transcript_grace(1_000), Duration::from_millis(500));
    for silence_ms in [0, 200, 1_000, 3_000, 30_000] {
        assert!(
            thinking_transcript_grace(silence_ms) * 2
                <= Duration::from_millis(u64::from(silence_ms))
                || thinking_transcript_grace(silence_ms) == THINKING_TRANSCRIPT_GRACE,
            "{silence_ms}"
        );
    }
}

#[test]
fn a_silent_declared_hold_ends_in_one_check_in() {
    let start = Instant::now();
    let limit = Duration::from_secs(crate::agent::THINKING_CHECK_IN_S);
    let second = Duration::from_secs(1);
    let mut state = RuntimeState {
        thinking_hold: ThinkingHold::Held { since: start },
        ..RuntimeState::default()
    };
    assert!(!state.claim_thinking_check_in(start, 0));
    assert!(!state.claim_thinking_check_in(start + limit - second, 0));

    // Paused, it cannot fire; resumed, the count starts again.
    state.paused = true;
    assert!(!state.claim_thinking_check_in(start + limit, 0));
    let resumed = start + limit;
    state.paused = false;
    state.restart_thinking_clock(resumed);
    assert!(!state.claim_thinking_check_in(resumed + limit - second, 0));
    assert!(state.claim_thinking_check_in(resumed + limit, 7));
    assert!(!state.thinking_hold.is_active());
    assert_eq!(state.evidence_ledger.lifecycle.transitions, 1);
    assert!(!state.claim_thinking_check_in(resumed + limit * 2, 8));

    // A provisional request is not a declared hold.
    state.thinking_hold = ThinkingHold::Requested {
        since: std::time::Instant::now(),
    };
    assert!(!state.claim_thinking_check_in(resumed + limit * 5, 9));
}

#[test]
fn a_new_hold_does_not_inherit_the_last_ones_clock() {
    let start = Instant::now();
    let limit = Duration::from_secs(crate::agent::THINKING_CHECK_IN_S);
    let mut state = RuntimeState {
        thinking_hold: ThinkingHold::Held { since: start },
        ..RuntimeState::default()
    };
    // Continue, then Thinking again, just short of the first hold's limit.
    let again = start + limit - Duration::from_secs(1);
    state.end_thinking(1);
    state.declare_thinking(again, 2);
    assert!(!state.claim_thinking_check_in(start + limit, 3));
    assert!(state.claim_thinking_check_in(again + limit, 4));
}

#[test]
fn a_resume_restarts_the_check_in_clock_through_the_reducer() {
    let mut state = RuntimeState {
        thinking_hold: ThinkingHold::Held {
            since: Instant::now() - Duration::from_secs(crate::agent::THINKING_CHECK_IN_S * 2),
        },
        ..RuntimeState::default()
    };
    for paused in [true, false] {
        crate::agent::apply_data_event(
            &mut state,
            crate::runtime::TOPIC_CONTROL,
            &serde_json::json!({"type":"pause_interview","paused":paused}),
            0.0,
        );
    }
    assert!(!state.claim_thinking_check_in(Instant::now(), 0));
}

#[test]
fn the_check_in_carries_what_the_hold_left_owed() {
    let state = RuntimeState {
        thinking_unheard_reply: true,
        owed_reply_on_resume: Some("Answer the outstanding candidate question.".into()),
        ..RuntimeState::default()
    };
    let prompt = crate::agent::thinking_check_in(&state);
    assert!(prompt.contains("Nothing you said while the candidate was thinking"));
    assert!(prompt.contains("Answer the outstanding candidate question."));
    assert!(prompt.contains("silent for 2 minutes"));
    assert!(prompt.contains("Do not give a hint"));
}

#[test]
fn continuing_while_a_held_reply_is_discarded_owes_one_reply_after_its_boundary() {
    let state = RuntimeState::default();
    let mut activity = RuntimeActivity::new(Instant::now());
    activity.discarding_output = true;
    activity.defer_thinking_reply("The five-minute warning is due. The candidate is ready.");
    assert!(activity.owes_reply());
    assert!(!activity.claim_thinking_reply(&state));
    let debt = activity.prompt_debt();
    activity.note_turn_boundary();
    activity.restore_prompt_debt(debt);
    activity.discarding_output = false;
    assert!(activity.claim_thinking_reply(&state));
    assert!(!activity.claim_thinking_reply(&state));
    assert!(
        activity
            .thinking_reply_prompt()
            .contains("five-minute warning")
    );
    let mut activity = RuntimeActivity::new(Instant::now());
    activity.defer_thinking_reply("Ready");
    assert!(!activity.claim_thinking_reply(&state));
}

#[test]
fn reconnect_only_declares_confirmed_thinking_requests() {
    let mut state = RuntimeState::default();
    let mut activity = RuntimeActivity::new(Instant::now());
    activity.observe_thinking_fragment(&mut state, "Wait", Instant::now(), 100);
    assert!(state.thinking_hold.is_active());
    assert!(!state.thinking_hold.is_declared());
    activity.observe_thinking_fragment(
        &mut state,
        "Wait, actually we sort first",
        Instant::now(),
        101,
    );
    assert!(!state.thinking_hold.is_declared());
    activity.observe_thinking_fragment(&mut state, "Wait", Instant::now(), 102);
    activity.confirm_thinking_request(&mut state, Instant::now(), 103);
    assert_eq!(state.take_thinking_notice(), Some(true));
    assert!(state.thinking_hold.is_declared());
}

#[test]
fn a_tentative_hold_does_not_repeat_an_already_answered_system_prompt() {
    let mut state = RuntimeState::default();
    let mut activity = RuntimeActivity::new(Instant::now());
    activity.mark_prompted(Instant::now(), Some("old five-minute warning"), false);
    activity.note_output();
    activity.note_turn_boundary();
    activity.discarding_output = true;
    activity.observe_thinking_fragment(&mut state, "Wait", Instant::now(), 100);
    let payload = serde_json::json!({"type":"yield_turn"});
    apply_control(&mut activity, &mut state, &payload);
    activity.discarding_output = false;
    assert!(activity.claim_thinking_reply(&state));
    assert!(!activity.owes_prompt());
    let prompt = activity.thinking_reply_prompt();
    assert!(!prompt.contains("old five-minute warning"));
}

#[test]
fn a_request_that_turns_into_reasoning_owes_no_unheard_note() {
    let mut state = RuntimeState::default();
    let mut activity = RuntimeActivity::new(Instant::now());
    activity.observe_thinking_fragment(&mut state, "Let me think", Instant::now(), 100);
    assert!(state.thinking_hold.is_active());
    state.thinking_unheard_reply = true;
    activity.observe_thinking_fragment(
        &mut state,
        "Let me think of an example: one",
        Instant::now(),
        200,
    );
    assert!(!state.thinking_hold.is_active());
    assert!(!state.thinking_unheard_reply);
}

/// What the room loop does with a control packet, minus the room: note
/// whether a request was pending, apply it, and settle that request.
fn apply_control(
    activity: &mut RuntimeActivity,
    state: &mut RuntimeState,
    payload: &serde_json::Value,
) -> crate::agent::DataEventResult {
    let was_requested = state.thinking_hold.is_requested();
    let result = crate::agent::apply_data_event(state, crate::runtime::TOPIC_CONTROL, payload, 0.0);
    activity.settle_thinking_request(was_requested, state, &result);
    result
}

#[test]
fn a_reply_owed_after_a_discard_waits_for_the_floor_and_is_dropped_by_the_ending() {
    let mut activity = RuntimeActivity::new(Instant::now());
    for held in [
        RuntimeState {
            paused: true,
            ..RuntimeState::default()
        },
        RuntimeState {
            thinking_hold: ThinkingHold::Held {
                since: Instant::now(),
            },
            ..RuntimeState::default()
        },
        RuntimeState {
            ended: true,
            ..RuntimeState::default()
        },
    ] {
        activity.reply_after_thinking_discard = true;
        assert!(!activity.claim_thinking_reply(&held));
        assert!(activity.reply_after_thinking_discard, "still owed");
    }
    assert!(activity.claim_thinking_reply(&RuntimeState::default()));
}

#[test]
fn confirming_needs_a_pending_request_in_a_live_interview() {
    let mut activity = RuntimeActivity::new(Instant::now());
    let mut state = RuntimeState::default();
    activity.confirm_thinking_request(&mut state, Instant::now(), 1);
    assert_eq!(state.thinking_hold, ThinkingHold::Off, "nothing to confirm");
    assert_eq!(state.take_thinking_notice(), None);

    state.thinking_hold = ThinkingHold::Requested {
        since: Instant::now(),
    };
    state.ended = true;
    activity.confirm_thinking_request(&mut state, Instant::now(), 2);
    assert!(
        state.thinking_hold.is_requested(),
        "an ended interview holds nothing"
    );
    assert_eq!(state.evidence_ledger.lifecycle.transitions, 0);
}

#[test]
fn the_watch_tick_declares_a_stale_request_and_drops_its_tentative_debt() {
    let asked = Instant::now();
    let mut state = RuntimeState {
        thinking_hold: ThinkingHold::Requested { since: asked },
        ..RuntimeState::default()
    };
    let mut activity = RuntimeActivity::new(asked);
    activity.reply_after_thinking_discard = true;
    let settled = asked + crate::agent::THINKING_REQUEST_SETTLE;
    activity.settle_stale_request(&mut state, settled - Duration::from_millis(1), 1);
    assert!(state.thinking_hold.is_requested());
    assert!(activity.reply_after_thinking_discard);
    activity.settle_stale_request(&mut state, settled, 2);
    assert!(state.thinking_hold.is_declared());
    assert!(!activity.reply_after_thinking_discard);
}
