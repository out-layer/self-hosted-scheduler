FROM rust:1.85-slim AS builder
WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/outlayer-scheduler /usr/local/bin/
ENTRYPOINT ["outlayer-scheduler", "--config", "/etc/outlayer/scheduler.toml"]
