# Talon Plugin

Observability tap and agent system for capturing and forwarding Claude Code hook events to a trace collector.

## Architecture

Talon consists of two cooperating binaries:

### `talon-tap` (Lightweight Hook Tap)

A minimal, fast binary designed to be called from Claude Code hooks. It:
- Reads JSON from stdin (with configurable size limits)
- Annotates events with metadata (timestamp, session ID, hostname, PID)
- Sends events to the agent via IPC (Unix socket on Unix, TCP on Windows)
- Auto-starts the agent if not running
- Exits quickly to avoid blocking the hook

**Environment Variables:**
- `TALON_TAP_MAX_STDIN_BYTES` - Max stdin bytes to read (default: 2MB)
- `TALON_SOCK` - IPC socket path (default: `/tmp/talon.sock`)
- `TALON_AGENT_PATH` - Path to talon-agent binary (default: `talon-agent`)
- `TALON_PLUGIN_VERSION` - Plugin version string for telemetry
- `CLAUDE_SESSION_ID` - Claude session identifier
- `TRACE_ENDPOINT` - Trace collector endpoint (passed to agent)
- `TRACE_API_KEY` - API key for trace collector (passed to agent)

### `talon-agent` (Background Agent)

A long-running daemon that:
- Accepts events from taps via IPC
- Batches events efficiently (by count, size, and time)
- Compresses batches with gzip
- Sends to trace collector with retries and exponential backoff
- Spools to disk on network failures
- Flushes spool opportunistically when network recovers

**Subcommands:**

#### `start`
Start the agent daemon:
```bash
talon-agent start \
    --endpoint https://collector.example.com/v1/traces \
    --api-key YOUR_API_KEY \
    --sock /tmp/talon.sock \
    --batch-size 100 \
    --batch-ms 200 \
    --batch-bytes 1048576 \
    --spool-bytes 50000000
```

**Options:**
- `--endpoint` - Trace collector HTTP endpoint (required)
- `--api-key` - Bearer token for authentication
- `--sock` - IPC socket path (default: `/tmp/talon.sock`)
- `--batch-size` - Max events per batch (default: 100)
- `--batch-ms` - Max milliseconds before flush (default: 200)
- `--chan-capacity` - Internal channel buffer size (default: 10,000)
- `--batch-bytes` - Max batch size in bytes (default: 1MB)
- `--spool-bytes` - Max spool file size (default: 50MB)
- `--spool-dir` - Spool directory (default: platform-specific)

#### `flush`
Manually flush spooled events:
```bash
talon-agent flush \
    --endpoint https://collector.example.com/v1/traces \
    --api-key YOUR_API_KEY
```

## Building

From this directory:

```bash
# Build both binaries
cargo build --release

# Build specific binary
cargo build --release --bin talon-tap
cargo build --release --bin talon-agent

# Binaries will be in target/release/
```

## Installation

```bash
# Copy binaries to PATH
cp target/release/talon-tap ~/.local/bin/
cp target/release/talon-agent ~/.local/bin/

# Or reference them directly in Claude Code hooks
```

## Claude Code Integration

### Hook Configuration

Add to your `.claude/hooks/`:

```bash
#!/bin/bash
# .claude/hooks/tool-call.sh

export TALON_SOCK="/tmp/talon.sock"
export TRACE_ENDPOINT="https://collector.example.com/v1/traces"
export TRACE_API_KEY="your-api-key"

# Read hook JSON from stdin and pipe to talon-tap
talon-tap --event "tool-call" < /dev/stdin
```

### Event Format

**Tap envelope** (sent via IPC from talon-tap to talon-agent):

```json
{
  "event": "tool-call",
  "payload": { /* original hook JSON */ },
  "ts": "2025-01-13T12:34:56.789Z",
  "env": {
    "session_id": "abc123",
    "host": "my-machine",
    "pid": 12345
  },
  "plugin": "talon",
  "version": "0.1.0"
}
```

**Note**: The agent transforms this to the canonical `beak.trace.v1` schema, adds trace/span IDs, then converts to Beak-compatible format before sending to the collector. See `schema.rs`, `map.rs`, and `beak_adapter.rs` for details.

## Design Decisions

### Why Two Binaries?

**Tap:** Optimized for minimal latency and resource usage in the hook hot path.

**Agent:** Handles complex batching, compression, retries, and spooling without blocking hooks.

### Why IPC?

Unix sockets (Unix) and TCP localhost (Windows) provide:
- Fast, low-overhead communication
- Decoupling: tap and agent can restart independently
- Backpressure: blocking sends slow down hooks rather than dropping data

### Batching Strategy

Multiple triggers:
- **Count:** Flush when batch reaches N events
- **Size:** Flush when batch reaches N bytes
- **Time:** Flush after N milliseconds

This balances latency and throughput.

### Failure Handling

- **Network failures:** Events spool to disk
- **Disk full:** Spool rotates (keeps last 50%)
- **Collector errors:**
  - 4xx: No retry (bad request)
  - 5xx: Exponential backoff with jitter
- **Agent not running:** Tap auto-starts agent

## Testing

```bash
# Run tap manually
echo '{"tool": "Read", "path": "/foo"}' | talon-tap --event test

# Start agent in foreground (verbose logging)
RUST_LOG=debug talon-agent start --endpoint http://localhost:8080/traces

# Verify socket exists
ls -l /tmp/talon.sock

# Check spool directory
ls -lh ~/.local/share/talon/spool/
```

## Platform Support

- **Unix (Linux, macOS):** Unix domain sockets
- **Windows:** TCP on `127.0.0.1:7878`

Both platforms share the same command-line interface and behavior.
