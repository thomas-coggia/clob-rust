use ws_session::state::{handle_event, ConnectionPhase, Effect, Event, SessionState};

 #[test]
 fn start_from_disconnected_connects() {
     let mut state = SessionState::new();
     assert_eq!(state.phase, ConnectionPhase::Disconnected);

     let effects = handle_event(&mut state, Event::Start);

     assert_eq!(state.phase, ConnectionPhase::Connecting);
     assert!(effects.contains(&Effect::Connect));
 }

 #[test]
 fn open_moves_to_subscribing_and_notifies() {
     let mut state = SessionState::new();
     let _ = handle_event(&mut state, Event::Start);
     assert_eq!(state.phase, ConnectionPhase::Connecting);

     let effects = handle_event(&mut state, Event::WsOpen);

     assert_eq!(state.phase, ConnectionPhase::Subscribing);
     assert!(effects.contains(&Effect::NotifyConnected));
 }

 #[test]
 fn json_message_is_forwarded_and_enters_running() {
     let mut state = SessionState {
         phase: ConnectionPhase::Subscribing,
         backoff: None,
     };

     let effects = handle_event(
         &mut state,
         Event::IncomingText(r#"{"event_type":"book"}"#.to_string()),
     );

     assert_eq!(state.phase, ConnectionPhase::Running);

     let has_forward = effects.iter().any(|e| matches!(e, Effect::ForwardJson(_)));
     assert!(has_forward);
 }

 #[test]
 fn error_triggers_reconnect_with_backoff() {
     let mut state = SessionState {
         phase: ConnectionPhase::Running,
         backoff: None,
     };

     let effects = handle_event(&mut state, Event::WsError);

     assert_eq!(state.phase, ConnectionPhase::Reconnecting);
     assert!(effects.iter().any(|e| matches!(e, Effect::NotifyDisconnected)));
     let backoff_effect = effects.iter().find_map(|e| {
         if let Effect::ScheduleBackoff { attempt } = e {
             Some(*attempt)
         } else {
             None
         }
     });
     assert_eq!(backoff_effect, Some(1));
 }

#[test]
fn close_triggers_reconnect_with_backoff() {
    let mut state = SessionState {
        phase: ConnectionPhase::Running,
        backoff: None,
    };

    let effects = handle_event(&mut state, Event::WsClosed);

    assert_eq!(state.phase, ConnectionPhase::Reconnecting);
    assert!(effects.iter().any(|e| matches!(e, Effect::NotifyDisconnected)));
    let backoff_effect = effects.iter().find_map(|e| {
        if let Effect::ScheduleBackoff { attempt } = e {
            Some(*attempt)
        } else {
            None
        }
    });
    assert_eq!(backoff_effect, Some(1));
}

#[test]
fn stop_resets_state_and_notifies() {
    let mut state = SessionState {
        phase: ConnectionPhase::Running,
        backoff: Some(ws_session::state::Backoff { attempt: 3 }),
    };

    let effects = handle_event(&mut state, Event::Stop);

    assert_eq!(state.phase, ConnectionPhase::Disconnected);
    assert!(state.backoff.is_none());
    assert!(effects.iter().any(|e| matches!(e, Effect::NotifyDisconnected)));
}

#[test]
fn invalid_json_is_ignored_and_phase_unchanged() {
    let mut state = SessionState {
        phase: ConnectionPhase::Subscribing,
        backoff: None,
    };

    let effects = handle_event(
        &mut state,
        Event::IncomingText("not-json".to_string()),
    );

    assert_eq!(state.phase, ConnectionPhase::Subscribing);
    assert!(effects.iter().any(|e| matches!(e, Effect::Ignore)));
}

#[test]
fn backoff_elapsed_reenters_connecting_and_requests_connect() {
    let mut state = SessionState {
        phase: ConnectionPhase::Reconnecting,
        backoff: Some(ws_session::state::Backoff { attempt: 2 }),
    };

    let effects = handle_event(&mut state, Event::BackoffElapsed);

    assert_eq!(state.phase, ConnectionPhase::Connecting);
    assert!(effects.iter().any(|e| matches!(e, Effect::Connect)));
}

