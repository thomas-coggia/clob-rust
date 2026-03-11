use crate::observer::SessionObserver;
use crate::state::{handle_event, Effect, Event, SessionState};
use futures_util::{Future, SinkExt, StreamExt};
use log::{error, info};
use std::pin::Pin;
use std::time::Duration;
use thiserror::Error;
use tokio::net::TcpStream;
use tokio::time;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

 #[derive(Debug, Clone)]
 pub struct SessionConfig {
     pub url: String,
     pub subscription_payload: String,
     pub max_backoff_secs: u64,
 }

 impl SessionConfig {
     pub fn polymarket_default() -> Self {
         let payload = r#"{
  "assets_ids": [
    "112493481455469093769281852159558847572704253342416714876781522096078968514094",
    "73624432805780182150964443951045800666977811185963019133914618974858599458273"
  ],
  "type": "market"
}"#
         .to_string();

         Self {
             url: "wss://ws-subscriptions-clob.polymarket.com/ws/market".to_string(),
             subscription_payload: payload,
             max_backoff_secs: 16,
         }
     }
 }

 #[derive(Debug, Error)]
 pub enum SessionRuntimeError {
     #[error("websocket error: {0}")]
     Ws(#[from] tokio_tungstenite::tungstenite::Error),
 }

 pub struct SessionRunner<O: SessionObserver + 'static> {
     config: SessionConfig,
     observer: O,
     state: SessionState,
 }

 impl<O: SessionObserver + 'static> SessionRunner<O> {
     pub fn new(config: SessionConfig, observer: O) -> Self {
         Self {
             config,
             observer,
             state: SessionState::new(),
         }
     }

    pub async fn run(self) -> Result<(), SessionRuntimeError> {
        self.run_inner::<futures_util::future::Pending<()>>(None).await
    }

    pub async fn run_with_shutdown<F>(self, shutdown: F) -> Result<(), SessionRuntimeError>
    where
        F: Future<Output = ()>,
    {
        self.run_inner(Some(Box::pin(shutdown))).await
    }

    async fn run_inner<F>(mut self, mut shutdown: Option<Pin<Box<F>>>) -> Result<(), SessionRuntimeError>
    where
        F: Future<Output = ()>,
    {
         // initial start event
         self.dispatch(Event::Start, None).await?;

        loop {
            // We only reach here when we need to (re)connect.
            let (ws_stream, _) = connect_async(&self.config.url).await?;
            let (mut sink, mut stream) = ws_stream.split();

            // Notify state machine that websocket is open.
            self.dispatch(Event::WsOpen, Some(&mut sink)).await?;

            // Send subscription immediately.
            sink.send(Message::Text(self.config.subscription_payload.clone()))
                .await?;

            // Client-side heartbeat: send PING text every 10 seconds.
            let mut ping_interval = time::interval(Duration::from_secs(10));

            // Main receive loop for this connection.
            let mut connection_alive = true;
            let mut shutdown_requested = false;

            while connection_alive && !shutdown_requested {
                match shutdown.as_mut() {
                    Some(shutdown_future) => {
                        tokio::select! {
                            msg = stream.next() => {
                                match msg {
                                    Some(Ok(Message::Text(text))) => {
                                        self.dispatch(Event::IncomingText(text), Some(&mut sink)).await?;
                                    }
                                    Some(Ok(Message::Ping(_))) => {
                                        sink.send(Message::Pong(vec![])).await?;
                                    }
                                    Some(Ok(Message::Close(_))) => {
                                        self.dispatch(Event::WsClosed, Some(&mut sink)).await?;
                                        connection_alive = false;
                                    }
                                    Some(Ok(_)) => {
                                        // ignore non-text frames
                                    }
                                    Some(Err(_e)) => {
                                        self.dispatch(Event::WsError, Some(&mut sink)).await?;
                                        connection_alive = false;
                                    }
                                    None => {
                                        self.dispatch(Event::WsClosed, Some(&mut sink)).await?;
                                        connection_alive = false;
                                    }
                                }
                            }
                            _ = ping_interval.tick() => {
                                // Polymarket market channel: client heartbeat as text "PING".
                                sink.send(Message::Text("PING".to_string())).await?;
                            }
                            _ = shutdown_future => {
                                // Request a graceful shutdown: finish current iteration,
                                // then break out of the loop.
                                info!("shutdown requested: will stop session after current iteration");
                                shutdown_requested = true;
                            }
                        }
                    }
                    None => {
                        tokio::select! {
                            msg = stream.next() => {
                                match msg {
                                    Some(Ok(Message::Text(text))) => {
                                        self.dispatch(Event::IncomingText(text), Some(&mut sink)).await?;
                                    }
                                    Some(Ok(Message::Ping(_))) => {
                                        sink.send(Message::Pong(vec![])).await?;
                                    }
                                    Some(Ok(Message::Close(_))) => {
                                        self.dispatch(Event::WsClosed, Some(&mut sink)).await?;
                                        connection_alive = false;
                                    }
                                    Some(Ok(_)) => {
                                        // ignore non-text frames
                                    }
                                    Some(Err(_e)) => {
                                        self.dispatch(Event::WsError, Some(&mut sink)).await?;
                                        connection_alive = false;
                                    }
                                    None => {
                                        self.dispatch(Event::WsClosed, Some(&mut sink)).await?;
                                        connection_alive = false;
                                    }
                                }
                            }
                            _ = ping_interval.tick() => {
                                // Polymarket market channel: client heartbeat as text "PING".
                                sink.send(Message::Text("PING".to_string())).await?;
                            }
                        }
                    }
                }
            }

            if shutdown_requested {
                // One final graceful stop notification to the state machine and observer.
                info!("shutting down session gracefully: dispatching Stop and closing websocket");
                let _ = self.dispatch(Event::Stop, Some(&mut sink)).await;
                // Best-effort close frame; ignore errors.
                if let Err(e) = sink.send(Message::Close(None)).await {
                    error!("failed to send websocket close frame during shutdown: {e}");
                }
                info!("session shutdown complete");
                return Ok(());
            }
        }
     }

    async fn dispatch(
        &mut self,
        event: Event,
        mut sink: Option<
            &mut futures_util::stream::SplitSink<
                WebSocketStream<MaybeTlsStream<TcpStream>>,
                Message,
            >,
        >,
    ) -> Result<(), SessionRuntimeError> {
        let effects = handle_event(&mut self.state, event);

        for effect in effects {
            match effect {
                Effect::Connect => {
                    // handled in run loop
                }
                Effect::SendText(text) => {
                    if let Some(sink_ref) = sink.as_deref_mut() {
                        sink_ref.send(Message::Text(text)).await?;
                    }
                }
                 Effect::ScheduleBackoff { attempt } => {
                     let secs = (1u64 << (attempt - 1)).min(self.config.max_backoff_secs);
                     time::sleep(Duration::from_secs(secs)).await;
                     let _ = handle_event(&mut self.state, Event::BackoffElapsed);
                 }
                 Effect::NotifyConnected => {
                     self.observer.on_connected();
                 }
                 Effect::NotifyDisconnected => {
                     self.observer.on_disconnected();
                 }
                 Effect::ForwardJson(value) => {
                     self.observer.on_json_message(value);
                 }
                 Effect::Ignore => {}
             }
         }

         Ok(())
     }
 }

