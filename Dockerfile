FROM rust:latest AS builder

WORKDIR /workspace

COPY . .

RUN cargo build --release --locked


FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /workspace/target/release/live .
COPY --from=builder /workspace/target/release/capture .
COPY --from=builder /workspace/target/release/replay .
COPY config.json .

ENTRYPOINT ["./live"]
CMD ["config.json"]