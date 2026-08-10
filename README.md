# OutLayer Scheduler

Config-driven scheduler for autonomous [OutLayer](https://app.outlayer.ai) agents. Runs on your server, triggers your WASI agent on schedule or in response to events.

## Architecture

The scheduler is a lightweight daemon that runs **outside TEE** on your infrastructure (VPS, laptop, Kubernetes). It decides **when** to call your agent and **what input** to pass. It never handles sensitive data — secrets, signing keys, and execution all stay inside the Intel TDX enclave.

This is the same design as `cron` in Linux: a simple, untrusted process that invokes kernel-protected processes.

```
Your Server (VPS)                    OutLayer (Phala Cloud / TEE)
┌──────────────────┐                 ┌──────────────────────────┐
│  Scheduler       │                 │  Intel TDX Enclave       │
│                  │   POST /call/   │                          │
│  Config (TOML)   │ ──────────────> │  Your WASI Agent         │
│  - interval      │   X-Payment-Key │  - fetches data          │
│  - storage diff  │                 │  - reads secrets          │
│  - webhook       │ <────────────── │  - writes to storage     │
│                  │   JSON response │  - calls contracts       │
└──────────────────┘                 │  - uses wallet           │
                                     └──────────────────────────┘
```

**Key principle**: The scheduler is untrusted infrastructure. It cannot read your secrets, modify execution results, or forge attestations. It can only trigger execution — the TEE guarantees everything else.

## Trigger Types

| Trigger | How it works | Use case |
|---------|-------------|----------|
| **Interval** | Calls agent every N seconds | Periodic refresh, heartbeat, maintenance |
| **Storage-diff** | Monitors public storage keys, triggers on value change | React to price moves, state changes, external updates |
| **Webhook** | Exposes HTTP endpoint, external services POST to trigger | GitHub events, Telegram bots, monitoring alerts |

All triggers can be combined. For example: interval every 60s for full refresh + storage-diff for immediate reaction to significant changes.

## Quick Start

### 1. Copy config

```bash
cp scheduler.example.toml scheduler.toml
cp .env.example .env
```

### 2. Configure your agent

Edit `scheduler.toml`:

```toml
[agent]
project_owner = "alice.near"
project_name = "my-agent"
payment_key = "${PAYMENT_KEY}"

[triggers]
interval_secs = 60

[input.static]
command = "tick"
```

### 3. Set secrets

Edit `.env`:

```env
PAYMENT_KEY=alice.near:1:your_secret_key_hex_here
```

Create a payment key via the [OutLayer dashboard](https://app.outlayer.ai) or CLI: `outlayer keys create`.

### 4. Run

**Docker (recommended):**

```bash
docker compose up -d
docker compose logs -f scheduler
```

**Direct:**

```bash
cargo run --release -- --config scheduler.toml
```

## Configuration Reference

### `[agent]` — Project identification

| Field | Required | Default | Description |
|-------|----------|---------|-------------|
| `project_owner` | yes | — | NEAR account owning the project |
| `project_name` | yes | — | OutLayer project name |
| `coordinator_url` | no | `https://api.outlayer.ai` | OutLayer API endpoint (**must be `https://`** — the payment key is sent in a header; redirects are not followed) |
| `payment_key` | yes | — | Payment key (`owner:nonce:secret`). Use `${PAYMENT_KEY}` to read from env |
| `secrets_profile` | no | — | Secrets profile name passed to WASI |
| `secrets_account_id` | no | — | NEAR account for secrets lookup |

### `[triggers]` — When to call your agent

| Field | Default | Description |
|-------|---------|-------------|
| `interval_secs` | `60` | Call agent every N seconds |

### `[triggers.storage_diff]` — React to storage changes

| Field | Default | Description |
|-------|---------|-------------|
| `enabled` | `false` | Enable storage-diff monitoring |
| `keys` | `[]` | Public storage keys to monitor |
| `project_uuid` | — | Project UUID for storage reads (required when enabled) |
| `threshold_percent` | `1.0` | For numeric values: trigger only if change exceeds N% |

The scheduler reads public storage via `POST /public/storage/batch` (no auth required), compares with the previous value, and triggers when:
- A non-numeric value changes (any change triggers)
- A numeric value changes by more than `threshold_percent`

On first startup, storage values are cached without triggering, to avoid false triggers.

### `[triggers.webhook]` — External HTTP triggers

| Field | Default | Description |
|-------|---------|-------------|
| `enabled` | `false` | Enable webhook HTTP server |
| `bind` | `127.0.0.1` | Bind address. **Local-only by default.** Set `0.0.0.0` to expose — a `secret` is then **required**. |
| `port` | `9090` | Port to listen on |
| `path` | `/trigger` | Path for trigger endpoint |
| `secret` | — | Shared secret (sent as `X-Webhook-Secret`). **Required** when `bind` is not loopback. |
| `max_per_minute` | `60` | Max accepted triggers per minute (each is a **paid** execution). `0` = unlimited. |

> ⚠️ **Each accepted POST triggers a paid execution** that spends your payment-key budget. The endpoint is loopback-only and rate-limited by default. To expose it to the network (or through Docker), set `bind = "0.0.0.0"` **and** a `secret` — the scheduler refuses to start an exposed, secret-less webhook.

When enabled, the scheduler starts an HTTP server with two endpoints:

**`POST {path}`** — trigger agent execution:

```bash
curl -X POST http://127.0.0.1:9090/trigger \
  -H "X-Webhook-Secret: your_secret" \
  -H "Content-Type: application/json" \
  -d '{"event": "new_order", "data": {"amount": 100}}'
```

The request body is passed to your agent as `webhook_data` in the input:

```json
{
  "command": "tick",
  "trigger": "webhook",
  "webhook_data": {"event": "new_order", "data": {"amount": 100}}
}
```

**`GET /health`** — health check:

```bash
curl http://your-server:9090/health
```

```json
{"status": "ok", "uptime_secs": 3600, "last_execution": "2026-03-07T10:30:00Z"}
```

### `[input]` — What to send to your agent

| Field | Default | Description |
|-------|---------|-------------|
| `[input.static]` | `{}` | Static key-value pairs included in every call |
| `include_trigger_reason` | `false` | Add `"trigger": "interval"/"storage_diff"/"webhook"` to input |

The `[input.static]` table is converted to JSON and sent as the agent's input. When `include_trigger_reason` is enabled, the trigger type and metadata are merged in:

```json
{
  "command": "tick",
  "trigger": "storage_diff",
  "changed_keys": ["price:wrap.near"]
}
```

### `[resources]` — Execution limits

| Field | Default | Description |
|-------|---------|-------------|
| `max_instructions` | `1000000000` | WASM instruction limit (1 billion) |
| `max_memory_mb` | `128` | Memory limit in MB |
| `max_execution_seconds` | `60` | Wall-clock timeout |
| `compute_limit` | `10000` | Compute budget in stablecoin micro-units ($0.01) |
| `attached_deposit` | `0` | Payment to project owner per call (micro-units) |

### `[alerts]` — Telegram notifications (optional)

| Field | Default | Description |
|-------|---------|-------------|
| `telegram_bot_token` | — | Telegram bot token |
| `telegram_chat_id` | — | Chat ID to send alerts to |
| `failure_threshold` | `3` | Alert after N consecutive failures |
| `alert_cooldown_secs` | `600` | Minimum seconds between repeated alerts |

Alerts are fully optional. When `telegram_bot_token` or `telegram_chat_id` are not set, all alerting is silently disabled.

When configured, the scheduler sends alerts on:
- **N consecutive execution failures** — includes error details
- Alerts are throttled (default 10 min cooldown) to prevent spam

### `[logging]`

| Field | Default | Description |
|-------|---------|-------------|
| `level` | `info` | Log level: `trace` / `debug` / `info` / `warn` / `error` |

Override with `RUST_LOG` environment variable.

### Environment variable expansion

Any value in the TOML config can reference an environment variable with `${VAR_NAME}` syntax:

```toml
payment_key = "${PAYMENT_KEY}"
```

This is resolved at startup from the process environment (including `.env` file loaded via dotenvy).

## Logs

The scheduler logs all decisions at `info` level:

```
INFO  Starting outlayer-scheduler v0.1.0
INFO  Project: alice.near/my-agent
INFO  Coordinator: https://api.outlayer.ai
INFO  Triggers: interval=60s, storage_diff=disabled, webhook=disabled
INFO  [interval] Triggering execution
INFO  [interval] Execution completed: status=completed, cost=1920 micro-units, time=230ms
```

Set `level = "debug"` or `RUST_LOG=debug` for detailed output including API request/response bodies.

```
WARN  [interval] Execution failed: WASI error: timeout exceeded
ERROR [interval] 3 consecutive failures, sending alert
```

## Deployment

### Docker Compose

```yaml
# docker-compose.yml
services:
  scheduler:
    build: .
    restart: unless-stopped
    env_file: .env
    volumes:
      - ./scheduler.toml:/etc/outlayer/scheduler.toml:ro
    ports:
      - "9090:9090"  # only if webhook trigger enabled
```

```bash
docker compose up -d
docker compose logs -f scheduler
```

### Dockerfile

```dockerfile
FROM rust:1.85-slim AS builder
WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/outlayer-scheduler /usr/local/bin/
ENTRYPOINT ["outlayer-scheduler", "--config", "/etc/outlayer/scheduler.toml"]
```

### Binary

```bash
cargo install --path .
outlayer-scheduler --config scheduler.toml
```

## API Interactions

The scheduler uses two OutLayer API endpoints:

### 1. Execute agent

```
POST https://api.outlayer.ai/call/{project_owner}/{project_name}

Headers:
  X-Payment-Key: owner:nonce:secret
  X-Compute-Limit: 10000
  X-Attached-Deposit: 0
  Content-Type: application/json

Body:
{
  "input": {"command": "tick", "trigger": "interval"},
  "secrets_ref": {"profile": "default", "account_id": "alice.near"},
  "resource_limits": {
    "max_instructions": 1000000000,
    "max_memory_mb": 128,
    "max_execution_seconds": 60
  },
  "async": false
}

Response:
{
  "status": "completed",
  "output": { ... },
  "compute_cost": "1920",
  "time_ms": 230
}
```

### 2. Read public storage (for storage-diff trigger)

```
POST https://api.outlayer.ai/public/storage/batch

Body:
{
  "project_uuid": "p0000000000000001",
  "keys": ["state:last_update", "data:price"]
}

Response:
{
  "results": {
    "state:last_update": {"exists": true, "value": "base64_encoded_data"},
    "data:price": {"exists": true, "value": "base64_encoded_data"}
  }
}
```

No authentication required — public storage is readable by anyone.

## Design Notes

- **Sequential execution** — triggers are processed one at a time via a mutex. This prevents concurrent agent calls that could cause race conditions in storage.
- **Graceful shutdown** — SIGINT/SIGTERM are handled cleanly.
- **No vendor lock-in** — replace the scheduler with a GitHub Action, a Lambda function, or a manual `curl` command. The agent doesn't know or care what triggered it.
