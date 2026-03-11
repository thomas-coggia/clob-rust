use std::fs::File;
use std::io::{BufWriter, Write};

use clap::Parser;
use log::{error, info, warn};
use serde_json::Value;
use tokio::signal;
use ws_session::config::AppConfig;
use ws_session::{
    build_subscription_payload, pin_current_thread_to_cpu, SessionConfig, SessionObserver,
    SessionObserverError, SessionRunner,
};

#[derive(Parser, Debug)]
#[command(name = "capture", about = "Capture live Polymarket feed as NDJSON")]
struct CaptureArgs {
    /// Path to JSON configuration file
    #[arg(value_name = "CONFIG", default_value = "config.json")]
    config: String,

    /// Output NDJSON file path
    #[arg(value_name = "OUTPUT", default_value = "capture.ndjson")]
    output: String,
}

struct CaptureObserver {
    writer: BufWriter<File>,
}

impl CaptureObserver {
    fn new(output_path: &str) -> Self {
        let file = File::create(output_path)
            .unwrap_or_else(|e| panic!("failed to create capture file {output_path}: {e}"));
        let writer = BufWriter::new(file);
        Self { writer }
    }
}

impl SessionObserver for CaptureObserver {
    fn on_connected(&mut self) {
        info!("capture: connected to Polymarket websocket");
    }

    fn on_disconnected(&mut self) {
        info!("capture: disconnected from Polymarket websocket");
        // Best-effort flush; ignore errors.
        let _ = self.writer.flush();
    }

    fn on_json_message(&mut self, value: Value) {
        // Write each JSON value as a single NDJSON line.
        if let Err(e) = serde_json::to_writer(&mut self.writer, &value) {
            error!("capture: failed to write JSON value: {e}");
        }
        if let Err(e) = self.writer.write_all(b"\n") {
            error!("capture: failed to write newline: {e}");
        }
    }

    fn on_error(&mut self, error: SessionObserverError) {
        error!("capture observer error: {error}");
    }
}

async fn shutdown_signal() {
    // Handle Ctrl-C.
    let ctrl_c = signal::ctrl_c();

    // Handle SIGTERM on Unix (e.g. Docker stop).
    #[cfg(unix)]
    let mut sigterm = {
        use tokio::signal::unix::{signal, SignalKind};
        signal(SignalKind::terminate()).expect("failed to install SIGTERM handler")
    };

    #[cfg(unix)]
    tokio::select! {
        _ = ctrl_c => {
            warn!("received SIGINT (Ctrl-C), requesting graceful shutdown (capture)");
        },
        _ = sigterm.recv() => {
            warn!("received SIGTERM, requesting graceful shutdown (capture)");
        },
    }

    #[cfg(not(unix))]
    {
        if ctrl_c.await.is_ok() {
            warn!("received Ctrl-C, requesting graceful shutdown (capture)");
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    env_logger::init();
    let args = CaptureArgs::parse();

    let contents = std::fs::read_to_string(&args.config)
        .unwrap_or_else(|e| panic!("failed to read config file {}: {e}", args.config));

    let app_config: AppConfig =
        serde_json::from_str(&contents).expect("failed to parse JSON config");

    if let Some(cpu_index) = app_config.cpu {
        // Best-effort: if pinning fails, just log and continue.
        if let Err(e) = pin_current_thread_to_cpu(cpu_index) {
            warn!("failed to set CPU affinity to {cpu_index}: {e}");
        }
    }

    let session_config = SessionConfig {
        url: app_config.url.clone(),
        subscription_payload: build_subscription_payload(&app_config.assets),
        max_backoff_secs: 16,
    };

    let observer = CaptureObserver::new(&args.output);
    let runner = SessionRunner::new(session_config, observer);

    if let Err(e) = runner.run_with_shutdown(shutdown_signal()).await {
        error!("capture session runtime error: {e}");
    }
}

