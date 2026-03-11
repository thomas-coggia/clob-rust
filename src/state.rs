 use serde_json::Value;

 #[derive(Debug, Clone, Copy, PartialEq, Eq)]
 pub enum ConnectionPhase {
     Disconnected,
     Connecting,
     Subscribing,
     Running,
     Reconnecting,
     Failed,
 }

 #[derive(Debug, Clone, Copy, PartialEq, Eq)]
 pub struct Backoff {
     pub attempt: u32,
}

 #[derive(Debug, Clone, PartialEq, Eq)]
 pub struct SessionState {
     pub phase: ConnectionPhase,
     pub backoff: Option<Backoff>,
 }

 impl SessionState {
     pub fn new() -> Self {
         Self {
             phase: ConnectionPhase::Disconnected,
             backoff: None,
         }
     }
 }

 #[derive(Debug, Clone, PartialEq, Eq)]
 pub enum Event {
     Start,
     Stop,
     WsOpen,
     WsClosed,
     WsError,
     BackoffElapsed,
     IncomingText(String),
 }

 #[derive(Debug, Clone, PartialEq)]
 pub enum Effect {
     Connect,
     SendText(String),
     ScheduleBackoff { attempt: u32 },
     NotifyConnected,
     NotifyDisconnected,
     ForwardJson(Value),
     Ignore,
 }

 pub fn handle_event(state: &mut SessionState, event: Event) -> Vec<Effect> {
     match (&state.phase, event) {
         (ConnectionPhase::Disconnected, Event::Start) => {
             state.phase = ConnectionPhase::Connecting;
             vec![Effect::Connect]
         }
         (ConnectionPhase::Connecting, Event::WsOpen) => {
             state.phase = ConnectionPhase::Subscribing;
             vec![
                 Effect::NotifyConnected,
                 // subscription message is injected by runtime
             ]
         }
         (ConnectionPhase::Subscribing, Event::WsClosed)
         | (ConnectionPhase::Running, Event::WsClosed)
         | (ConnectionPhase::Subscribing, Event::WsError)
         | (ConnectionPhase::Running, Event::WsError) => {
             let attempt = state.backoff.as_ref().map_or(1, |b| b.attempt + 1);
             state.phase = ConnectionPhase::Reconnecting;
             state.backoff = Some(Backoff { attempt });
             vec![
                 Effect::NotifyDisconnected,
                 Effect::ScheduleBackoff { attempt },
             ]
         }
         (ConnectionPhase::Reconnecting, Event::BackoffElapsed) => {
             state.phase = ConnectionPhase::Connecting;
             vec![Effect::Connect]
         }
        (_, Event::Stop) => {
            state.phase = ConnectionPhase::Disconnected;
            state.backoff = None;
            vec![Effect::NotifyDisconnected]
        }
        // Incoming text classification
        (ConnectionPhase::Subscribing, Event::IncomingText(t))
        | (ConnectionPhase::Running, Event::IncomingText(t)) => {
            let trimmed = t.trim();

            // Heartbeats:
            // - Market/User channels: we send "PING", server replies "PONG"
            // - Sports channel: server sends "ping", we must reply "pong"
            if trimmed == "PING" {
                return vec![Effect::SendText("PONG".to_string())];
            }
            if trimmed == "ping" {
                return vec![Effect::SendText("pong".to_string())];
            }

            // Try to parse JSON; on success forward, else ignore
            match serde_json::from_str::<Value>(&t) {
                Ok(value) => {
                    // As soon as we see valid JSON market messages, treat as running
                    state.phase = ConnectionPhase::Running;
                    vec![Effect::ForwardJson(value)]
                }
                Err(_) => vec![Effect::Ignore],
            }
        }
         _ => Vec::new(),
     }
 }

