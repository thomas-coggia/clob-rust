## Polymarket Order Book Session

Single-threaded async WebSocket client for Polymarket CLOB feeds. Maintains in-memory order books, prints a rolling top-of-book view, and serves a live dashboard over HTTP.

## Development

```bash
cargo build
cargo test
RUST_LOG=info cargo run --bin live -- config.json -v
```

## Docker

```bash
docker build -t ws-session .
```

### Live feed

Connects to Polymarket, prints a rolling top-of-book view, and serves a dashboard at `http://localhost:8080/`.

```bash
docker run --rm \
  -v "$PWD/config.json":/app/config.json:ro \
  -p 8080:8080 \
  -e RUST_LOG=info \
  ws-session -v /app/config.json
```

### Capture

Records the raw feed to a local NDJSON file for later replay.

```bash
docker run --rm \
  -v "$PWD/config.json":/app/config.json:ro \
  -v "$PWD":/data \
  -e RUST_LOG=info \
  --entrypoint /app/capture \
  ws-session /app/config.json /data/capture.ndjson
```

### Replay

Replays a captured file through the same order book observer as live mode.

```bash
docker run --rm \
  -v "$PWD/config.json":/app/config.json:ro \
  -v "$PWD":/data \
  -e RUST_LOG=info \
  --entrypoint /app/replay \
  ws-session /app/config.json /data/capture.ndjson
```

See [`ARCHITECTURE.md`](ARCHITECTURE.md) for an overview of the codebase.
