use std::fs;
use std::path::Path;

use clap::Parser;
use log::{error, info, warn};
use serde_json::Value;
use tokio::time::{sleep, Duration};
use ws_session::config::AppConfig;
use ws_session::observer::{AssetConfig, ObserverConfig, OrderbookObserver};
use ws_session::SessionObserver;
use ws_session::SkipOrderbook;

#[derive(Parser, Debug)]
#[command(name = "replay", about = "Replay captured NDJSON feed through the order book observer")]
struct ReplayArgs {
    /// Path to JSON configuration file
    #[arg(value_name = "CONFIG", default_value = "config.json")]
    config: String,

    /// Input NDJSON file path
    #[arg(value_name = "INPUT", default_value = "capture.ndjson")]
    input: String,

    /// Enable verbose pretty-printed order books on stdout
    #[arg(short = 'v', long = "verbose")]
    verbose: bool,
}

async fn run_replay<P: AsRef<Path>>(
    config_path: P,
    ndjson_path: P,
    verbose: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let config_path_str = config_path.as_ref().to_string_lossy().to_string();
    let ndjson_path_str = ndjson_path.as_ref().to_string_lossy().to_string();

    let contents = fs::read_to_string(&config_path_str)
        .unwrap_or_else(|e| panic!("failed to read config file {config_path_str}: {e}"));

    let app_config: AppConfig =
        serde_json::from_str(&contents).expect("failed to parse JSON config");

    let observer_config = ObserverConfig {
        assets: app_config
            .assets
            .into_iter()
            .map(|a| AssetConfig {
                id: a.id,
                label: a.label,
            })
            .collect(),
        notifier: None,
        verbose,
    };

    let mut observer = OrderbookObserver::<SkipOrderbook>::new(observer_config);

    let ndjson_contents = fs::read_to_string(&ndjson_path_str)
        .unwrap_or_else(|e| panic!("failed to open NDJSON file {ndjson_path_str}: {e}"));

    info!("starting replay from `{ndjson_path_str}` using config `{config_path_str}`");

    for line in ndjson_contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        match serde_json::from_str::<Value>(trimmed) {
            Ok(value) => {
                observer.on_json_message(value);
                // Optional: small delay to avoid flooding stdout.
                sleep(Duration::from_millis(1)).await;
            }
            Err(e) => {
                warn!("replay: failed to parse NDJSON line as JSON: {e}");
            }
        }
    }

    info!("replay: reached end of NDJSON file");
    info!("replay: completed");
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    env_logger::init();
    let args = ReplayArgs::parse();

    if let Err(e) = run_replay(&args.config, &args.input, args.verbose).await {
        error!("replay error: {e}");
    }
}


