use std::fs;

use clap::Parser;
use tokio::runtime::Builder;
use tokio::signal;
use tokio::net::TcpListener;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::broadcast;
use log::{info, warn};
use ws_session::config::AppConfig;
use ws_session::observer::{AssetConfig, ObserverConfig, OrderbookObserver, TopBookSnapshot};
use ws_session::FastOrderbook;
use serde_json::json;
use ws_session::{
    build_subscription_payload, pin_current_thread_to_cpu, SessionConfig, SessionRunner,
};

#[derive(Parser, Debug)]
#[command(name = "live", about = "Run live Polymarket order book session")]
struct LiveArgs {
    /// Path to JSON configuration file
    #[arg(value_name = "CONFIG", default_value = "config.json")]
    config: String,

    /// Enable verbose pretty-printed order books on stdout
    #[arg(short = 'v', long = "verbose")]
    verbose: bool,
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
            info!("received SIGINT (Ctrl-C), requesting graceful shutdown");
        },
        _ = sigterm.recv() => {
            info!("received SIGTERM, requesting graceful shutdown");
        },
    }

    #[cfg(not(unix))]
    {
        if ctrl_c.await.is_ok() {
            info!("received Ctrl-C, requesting graceful shutdown");
        }
    }
}

async fn async_main(config_path: String, verbose: bool) {
    let contents = fs::read_to_string(&config_path)
        .unwrap_or_else(|e| panic!("failed to read config file {config_path}: {e}"));

    let app_config: AppConfig =
        serde_json::from_str(&contents).expect("failed to parse JSON config");

    if let Some(cpu_index) = app_config.cpu {
        if let Err(e) = pin_current_thread_to_cpu(cpu_index) {
            eprintln!("failed to set CPU affinity to {cpu_index}: {e}");
        }
    }

    let session_config = SessionConfig {
        url: app_config.url.clone(),
        subscription_payload: build_subscription_payload(&app_config.assets),
        max_backoff_secs: 16,
    };

    let (notifier_tx, notifier_rx) = broadcast::channel::<TopBookSnapshot>(16);

    // Spawn a small HTTP server that serves the latest formatted order book view.
    // Bind on 0.0.0.0 so it is reachable from outside a Docker container.
    tokio::spawn(run_http_server("0.0.0.0:8080".to_string(), notifier_rx));

    let observer_config = ObserverConfig {
        assets: app_config
            .assets
            .into_iter()
            .map(|a| AssetConfig {
                id: a.id,
                label: a.label,
            })
            .collect(),
        notifier: Some(notifier_tx),
        verbose,
    };

    let observer = OrderbookObserver::<FastOrderbook>::new(observer_config);

    let runner = SessionRunner::new(session_config, observer);

    if let Err(e) = runner.run_with_shutdown(shutdown_signal()).await {
        warn!("session runtime error: {e}");
    }
}

async fn run_http_server(addr: String, mut rx: broadcast::Receiver<TopBookSnapshot>) {
    let listener = match TcpListener::bind(&addr).await {
        Ok(listener) => {
            info!("HTTP server listening on http://{addr}");
            listener
        }
        Err(e) => {
            warn!("failed to bind HTTP server on {addr}: {e}");
            return;
        }
    };

    let mut latest_snapshot: Option<TopBookSnapshot> = None;

    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                let (mut socket, _) = match accept_result {
                    Ok(pair) => pair,
                    Err(e) => {
                        warn!("HTTP server accept error: {e}");
                        continue;
                    }
                };

                let mut buf = [0u8; 1024];
                let n = match socket.read(&mut buf).await {
                    Ok(n) if n > 0 => n,
                    _ => continue,
                };

                let request = String::from_utf8_lossy(&buf[..n]);
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("/");

                let (status_line, content_type, body) = if path == "/api/books" {
                    let snapshot_json = match &latest_snapshot {
                        Some(snapshot) => serde_json::to_string(snapshot)
                            .unwrap_or_else(|_| json!({ "assets": [] }).to_string()),
                        None => json!({ "assets": [] }).to_string(),
                    };
                    (
                        "HTTP/1.1 200 OK",
                        "application/json; charset=utf-8",
                        snapshot_json,
                    )
                } else {
                    let html = build_orderbook_html_page();
                    (
                        "HTTP/1.1 200 OK",
                        "text/html; charset=utf-8",
                        html,
                    )
                };

                let response = format!(
                    "{status_line}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );

                tokio::spawn(async move {
                    if let Err(e) = socket.write_all(response.as_bytes()).await {
                        warn!("HTTP server write error: {e}");
                    }
                });
            }
            recv_result = rx.recv() => {
                match recv_result {
                    Ok(snapshot) => {
                        latest_snapshot = Some(snapshot);
                    }
                    Err(e) => {
                        warn!("HTTP notifier receive error: {e}");
                        // If all senders are dropped, there is no point in continuing.
                        if e == broadcast::error::RecvError::Closed {
                            break;
                        }
                    }
                }
            }
        }
    }
}

fn build_orderbook_html_page() -> String {
    // Single-page UI that fetches /api/books every second and renders a styled dashboard.
    r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8" />
  <title>Polymarket Order Books</title>
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <style>
    :root {
      --bg: #050816;
      --bg-elevated: rgba(15, 23, 42, 0.96);
      --accent: #22d3ee;
      --accent-soft: rgba(34, 211, 238, 0.18);
      --danger: #f97373;
      --text-main: #e5e7eb;
      --text-muted: #9ca3af;
      --border-subtle: rgba(148, 163, 184, 0.35);
      --shadow-soft: 0 18px 45px rgba(0, 0, 0, 0.55);
      --radius-card: 18px;
      --radius-pill: 999px;
    }

    * {
      box-sizing: border-box;
      margin: 0;
      padding: 0;
    }

    body {
      min-height: 100vh;
      font-family: system-ui, -apple-system, BlinkMacSystemFont, "SF Pro Text",
        "Segoe UI", sans-serif;
      color: var(--text-main);
      background:
        radial-gradient(circle at top left, #1d4ed8 0, transparent 55%),
        radial-gradient(circle at bottom right, #06b6d4 0, transparent 55%),
        radial-gradient(circle at center, #0f172a 0, #020617 55%);
      background-attachment: fixed;
      display: flex;
      align-items: stretch;
      justify-content: center;
      padding: 32px 16px;
    }

    .shell {
      width: 100%;
      max-width: 1200px;
      background: linear-gradient(135deg, rgba(15,23,42,0.98), rgba(8,17,40,0.98));
      border-radius: 28px;
      border: 1px solid rgba(148, 163, 184, 0.35);
      box-shadow: var(--shadow-soft);
      padding: 20px 22px 24px;
      backdrop-filter: blur(26px) saturate(1.4);
      position: relative;
      overflow: hidden;
    }

    .shell::before {
      content: "";
      position: absolute;
      inset: -120px;
      background:
        radial-gradient(circle at 10% -10%, rgba(59,130,246,0.22), transparent 55%),
        radial-gradient(circle at 110% 110%, rgba(56,189,248,0.25), transparent 55%);
      opacity: 0.7;
      mix-blend-mode: screen;
      pointer-events: none;
    }

    .chrome {
      position: relative;
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 18px;
      margin-bottom: 18px;
    }

    .dots {
      display: flex;
      gap: 8px;
      align-items: center;
    }

    .dot {
      width: 11px;
      height: 11px;
      border-radius: 999px;
      background: radial-gradient(circle at 30% 25%, rgba(255,255,255,0.9), transparent 55%),
                  #f97373;
      box-shadow: 0 0 0 1px rgba(0,0,0,0.45), 0 0 18px rgba(248, 113, 113, 0.35);
    }

    .dot:nth-child(2) {
      background: radial-gradient(circle at 30% 25%, rgba(255,255,255,0.9), transparent 55%),
                  #facc15;
      box-shadow: 0 0 0 1px rgba(0,0,0,0.45), 0 0 18px rgba(250,204,21,0.35);
    }

    .dot:nth-child(3) {
      background: radial-gradient(circle at 30% 25%, rgba(255,255,255,0.9), transparent 55%),
                  #4ade80;
      box-shadow: 0 0 0 1px rgba(0,0,0,0.45), 0 0 18px rgba(74,222,128,0.35);
    }

    .title {
      position: relative;
      display: flex;
      flex-direction: column;
      gap: 4px;
    }

    .title-main {
      font-size: 18px;
      letter-spacing: 0.08em;
      text-transform: uppercase;
      font-weight: 600;
      color: #e5e7eb;
      display: inline-flex;
      align-items: baseline;
      gap: 8px;
    }

    .title-main span {
      font-size: 11px;
      font-weight: 500;
      text-transform: uppercase;
      letter-spacing: 0.16em;
      color: var(--text-muted);
    }

    .title-sub {
      font-size: 12px;
      color: var(--text-muted);
    }

    .pill {
      position: relative;
      padding: 4px 12px;
      border-radius: var(--radius-pill);
      border: 1px solid rgba(148, 163, 184, 0.7);
      font-size: 11px;
      color: var(--text-muted);
      display: inline-flex;
      align-items: center;
      gap: 6px;
      background: radial-gradient(circle at 0 0, rgba(34,211,238,0.18), transparent 65%);
    }

    .pill-dot {
      width: 7px;
      height: 7px;
      border-radius: 999px;
      background: radial-gradient(circle at 30% 20%, #bbf7d0, #22c55e);
      box-shadow: 0 0 12px rgba(22,163,74,0.85);
    }

    .grid {
      position: relative;
      margin-top: 6px;
      display: grid;
      grid-template-columns: repeat(auto-fit, minmax(260px, 1fr));
      gap: 18px;
      z-index: 1;
    }

    .card {
      border-radius: var(--radius-card);
      border: 1px solid var(--border-subtle);
      background: radial-gradient(circle at top left, rgba(15,23,42,0.9), rgba(17,24,39,0.97));
      padding: 16px 16px 14px;
      box-shadow: 0 16px 40px rgba(0,0,0,0.55);
      position: relative;
      overflow: hidden;
    }

    .card::before {
      content: "";
      position: absolute;
      inset: -40%;
      background:
        radial-gradient(circle at top right, rgba(34,211,238,0.18), transparent 55%),
        radial-gradient(circle at bottom left, rgba(59,130,246,0.16), transparent 60%);
      opacity: 0.65;
      mix-blend-mode: soft-light;
      pointer-events: none;
    }

    .card-header {
      position: relative;
      display: flex;
      align-items: center;
      justify-content: space-between;
      margin-bottom: 12px;
    }

    .card-title {
      font-size: 14px;
      font-weight: 600;
      letter-spacing: 0.08em;
      text-transform: uppercase;
      color: #e5e7eb;
    }

    .card-subtitle {
      font-size: 11px;
      color: var(--text-muted);
    }

    .badge {
      font-size: 10px;
      text-transform: uppercase;
      letter-spacing: 0.16em;
      border-radius: var(--radius-pill);
      padding: 3px 10px;
      background: rgba(15, 23, 42, 0.82);
      border: 1px solid rgba(148, 163, 184, 0.65);
      color: var(--text-muted);
    }

    .spread-row {
      position: relative;
      display: flex;
      align-items: baseline;
      justify-content: space-between;
      margin-bottom: 8px;
      font-size: 11px;
      color: var(--text-muted);
      gap: 8px;
    }

    .spread-row strong {
      font-size: 13px;
      color: var(--accent);
    }

    .spread-label {
      font-size: 10px;
      letter-spacing: 0.12em;
      text-transform: uppercase;
      color: var(--text-muted);
    }

    .table {
      position: relative;
      width: 100%;
      border-collapse: collapse;
      font-size: 11px;
      margin-top: 4px;
    }

    .table thead {
      color: var(--text-muted);
    }

    .table th {
      font-weight: 500;
      text-transform: uppercase;
      letter-spacing: 0.12em;
      font-size: 10px;
      padding-bottom: 5px;
    }

    .table th,
    .table td {
      padding-right: 8px;
      white-space: nowrap;
    }

    .table tbody tr {
      transition: background 180ms ease-out, transform 180ms ease-out;
    }

    .table tbody tr:nth-child(even) {
      background: rgba(15, 23, 42, 0.7);
    }

    .table tbody tr:hover {
      background: rgba(30, 64, 175, 0.35);
      transform: translateY(-1px);
    }

    .bid {
      color: #4ade80;
    }

    .ask {
      color: var(--danger);
    }

    .muted {
      color: var(--text-muted);
    }

    .status-line {
      position: relative;
      margin-top: 18px;
      display: flex;
      justify-content: space-between;
      align-items: center;
      font-size: 11px;
      color: var(--text-muted);
      gap: 18px;
    }

    .status-line strong {
      color: var(--accent);
      font-weight: 500;
    }

    .status-ghost {
      opacity: 0.78;
    }

    @media (max-width: 640px) {
      .shell {
        padding: 16px 14px 18px;
        border-radius: 22px;
      }
      .title-main {
        font-size: 15px;
      }
      .grid {
        gap: 14px;
      }
    }
  </style>
</head>
<body>
  <main class="shell">
    <header class="chrome">
      <div class="dots">
        <div class="dot"></div>
        <div class="dot"></div>
        <div class="dot"></div>
      </div>
      <div class="title">
        <div class="title-main">
          Polymarket Order Books
          <span>Live Top&nbsp;5 Levels</span>
        </div>
        <div class="title-sub">
          Single-threaded feed · Zero mutexes · WebSocket → HTTP fan-out
        </div>
      </div>
      <div class="pill">
        <span class="pill-dot"></span>
        <span id="connection-label">Feed: live</span>
      </div>
    </header>

    <section id="grid" class="grid">
      <!-- Cards injected here -->
    </section>

    <footer class="status-line">
      <div class="status-ghost">
        Last update: <strong id="last-update">–</strong>
      </div>
      <div class="status-ghost">
        Source: WebSocket snapshot → HTTP `/api/books`
      </div>
    </footer>
  </main>

  <script>
    const grid = document.getElementById("grid");
    const lastUpdateEl = document.getElementById("last-update");

    function formatTime(ts) {
      const d = new Date(ts);
      return d.toLocaleTimeString(undefined, {
        hour12: false,
        hour: "2-digit",
        minute: "2-digit",
        second: "2-digit",
      });
    }

    function render(snapshot) {
      grid.innerHTML = "";
      if (!snapshot || !snapshot.assets || snapshot.assets.length === 0) {
        const empty = document.createElement("div");
        empty.className = "card";
        empty.innerHTML = "<div class='card-header'><div><div class='card-title'>Waiting for books…</div><div class='card-subtitle'>No snapshots published yet.</div></div></div>";
        grid.appendChild(empty);
        return;
      }

      snapshot.assets.forEach((asset) => {
        const card = document.createElement("article");
        card.className = "card";

        const spread = asset.spread || "–";
        const bestBid = asset.best_bid || "–";
        const bestAsk = asset.best_ask || "–";

        card.innerHTML = `
          <div class="card-header">
            <div>
              <div class="card-title">${asset.label}</div>
              <div class="card-subtitle">${asset.asset_id}</div>
            </div>
            <div class="badge">Top 5 Levels</div>
          </div>
          <div class="spread-row">
            <div>
              <div class="spread-label">Spread</div>
              <strong>${spread}</strong>
            </div>
            <div class="muted">
              Bid&nbsp;<span class="bid">${bestBid}</span>
              &nbsp;&middot;&nbsp;
              Ask&nbsp;<span class="ask">${bestAsk}</span>
            </div>
          </div>
          <table class="table">
            <thead>
              <tr>
                <th colspan="3" style="text-align:left">Bids</th>
                <th></th>
                <th colspan="3" style="text-align:left">Asks</th>
              </tr>
              <tr>
                <th class="muted">Price</th>
                <th class="muted">Size</th>
                <th class="muted">Cumul.</th>
                <th></th>
                <th class="muted">Price</th>
                <th class="muted">Size</th>
                <th class="muted">Cumul.</th>
              </tr>
            </thead>
            <tbody>
              ${Array.from({ length: Math.max(asset.bids.length, asset.asks.length, 5) })
                .map((_, i) => {
                  const bid = asset.bids[i];
                  const ask = asset.asks[i];
                  return `
                    <tr>
                      <td class="bid">${bid ? bid.price : ""}</td>
                      <td class="muted">${bid ? bid.size : ""}</td>
                      <td class="muted">${bid ? bid.cumulative : ""}</td>
                      <td></td>
                      <td class="ask">${ask ? ask.price : ""}</td>
                      <td class="muted">${ask ? ask.size : ""}</td>
                      <td class="muted">${ask ? ask.cumulative : ""}</td>
                    </tr>
                  `;
                })
                .join("")}
            </tbody>
          </table>
        `;

        grid.appendChild(card);
      });

      lastUpdateEl.textContent = formatTime(Date.now());
    }

    async function poll() {
      try {
        const res = await fetch("/api/books", { cache: "no-store" });
        if (!res.ok) throw new Error("HTTP " + res.status);
        const data = await res.json();
        render(data);
      } catch (err) {
        console.error("Failed to fetch books:", err);
      }
    }

    poll();
    setInterval(poll, 1000);
  </script>
</body>
</html>
"#.to_string()
}

fn main() {
    env_logger::init();
    let args = LiveArgs::parse();

    // Run everything on a single-threaded Tokio runtime.
    let runtime = Builder::new_current_thread()
        .enable_time()
        .enable_io()
        .build()
        .expect("failed to build Tokio current-thread runtime");

    runtime.block_on(async_main(args.config, args.verbose));
}

