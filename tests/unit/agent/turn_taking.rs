use super::*;

#[test]
fn explicit_requests_keep_the_floor() {
    for request in [
        "Let me think for a moment.",
        "I'd sort first. Hmm, let me think.",
        "Would sorting work? Please give me some time.",
        "Please give me some time.",
        "Give me a minute to work this out",
        "I need a moment to think",
        "Please wait.",
        "Wait, let me think.",
        "Wait a second.",
        "Um, wait, give me a minute",
        "I would use a hash map, please give me some time",
        "I would use a hash map please give me some time",
        "I'm not sure yet, let me think about it",
        "Can you give me some time?",
        "Could you please give me a moment?",
        "Can I have a minute?",
        "Hold on.",
        "One second, please.",
        "Okay so let me think this through",
        "Could I take a minute?",
        "May I take some time?",
        "I need a bit",
        "Give me a bit more time",
        "I need a few minutes",
        "Hmm, let me see",
        "Okay, one second.",
        "Hold on, please.",
    ] {
        assert!(requests_thinking_time(request), "{request}");
    }
    for answer in [
        "I think I would use a hash map to store the index",
        "Wait a second, that's wrong",
        "The user might say let me think",
        "I would sort first. The user might say let me think.",
        "Do not give me some time",
        "I don't need time to think",
        "Let me think of an example: one, two, three",
        "The loop should wait",
        "It only takes one second",
        "Then I would hold on to the previous index",
        "Can you give me some hints",
        "The user could say please give me some time and leave",
        "The timeout is, uh, one second",
        "So we just hold on",
        "Then the thread should wait",
        "Let me see if this works",
    ] {
        assert!(!requests_thinking_time(answer), "{answer}");
    }
}

#[test]
fn fragmented_requests_release_when_the_candidate_keeps_reasoning() {
    for fragments in [
        vec!["Let me think", " of an example", ": one, two, three"],
        vec![
            "Let me think",
            " for a moment",
            ". Okay, I would use a hash map",
        ],
        vec!["Okay, let me think", " about the edge cases"],
    ] {
        let mut text = String::new();
        let mut hold = ThinkingHold::Off;
        for fragment in fragments {
            text.push_str(fragment);
            hold = follow(hold, &text);
        }
        assert_eq!(hold, ThinkingHold::Off, "reasoning was held: {text}");
    }
    let mut hold = ThinkingHold::Off;
    let mut text = String::new();
    for fragment in ["Hmm, give me", " a moment", " to think", ", please"] {
        text.push_str(fragment);
        hold = follow(hold, &text);
    }
    // The last phrase includes a polite suffix after a complete request.
    assert!(hold.is_requested());
    let held = ThinkingHold::Held {
        since: std::time::Instant::now(),
    };
    assert_eq!(thinking_change(held, "hmm"), None);
    assert_eq!(thinking_change(held, "um uh"), None);
    assert_eq!(thinking_change(held, "Let me think"), None);
    assert_eq!(thinking_change(held, "Ready"), Some(false));
    assert_eq!(thinking_change(held, "I would use a hash map"), Some(false));
}

/// The hold the room loop's reading of each fragment leaves, until the
/// utterance ends.
fn follow(hold: ThinkingHold, text: &str) -> ThinkingHold {
    match thinking_change(hold, text) {
        Some(true) => ThinkingHold::Requested {
            since: std::time::Instant::now(),
        },
        Some(false) => ThinkingHold::Off,
        None => hold,
    }
}

#[test]
fn the_page_is_told_each_declared_change_once() {
    let now = std::time::Instant::now();
    let mut state = RuntimeState::default();
    state.request_thinking();
    assert_eq!(
        state.take_thinking_notice(),
        None,
        "a request is not public"
    );
    assert!(state.withdraw_thinking_request());
    assert_eq!(state.take_thinking_notice(), None);
    assert!(state.declare_thinking(now, 1));
    assert_eq!(state.take_thinking_notice(), Some(true));
    assert_eq!(state.take_thinking_notice(), None);
    assert!(!state.declare_thinking(now, 2));
    assert_eq!(state.take_thinking_notice(), None);
    state.announce_thinking();
    assert_eq!(state.take_thinking_notice(), Some(true));
    assert!(state.floor_held());
    assert!(state.end_thinking(3));
    assert_eq!(state.take_thinking_notice(), Some(false));
    assert!(!state.floor_held());
    assert!(!state.end_thinking(4));
    assert_eq!(state.take_thinking_notice(), None);
    assert!(!state.withdraw_thinking_request());
}

#[test]
fn a_request_nothing_decides_is_declared_once_it_settles() {
    let asked = std::time::Instant::now();
    let mut state = RuntimeState {
        thinking_hold: ThinkingHold::Requested { since: asked },
        ..RuntimeState::default()
    };
    let settled = asked + THINKING_REQUEST_SETTLE;
    assert!(!state.settle_stale_request(settled - std::time::Duration::from_millis(1), 1));
    state.paused = true;
    assert!(!state.settle_stale_request(settled, 1), "not while paused");
    state.paused = false;
    assert!(state.settle_stale_request(settled, 2));
    assert!(state.thinking_hold.is_declared());
    assert_eq!(state.take_thinking_notice(), Some(true));
    assert_eq!(state.evidence_ledger.lifecycle.transitions, 1);
    assert!(
        !state.settle_stale_request(settled, 3),
        "only a request settles"
    );
}

#[test]
fn the_release_cooldown_ends_exactly_at_its_length() {
    let released = std::time::Instant::now();
    let mut state = RuntimeState::default();
    assert!(!state.released_recently(released));
    state.thinking_released_at = Some(released);
    assert!(state.released_recently(released));
    let cooldown = crate::agent::THINKING_RELEASE_COOLDOWN;
    assert!(state.released_recently(released + cooldown - std::time::Duration::from_millis(1)));
    assert!(!state.released_recently(released + cooldown));
}
