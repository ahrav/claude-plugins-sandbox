# Beak Tracer Plugin - Setup Guide

> **NOTE:** This guide covers setup for the **beak-tracer** plugin, which is part of an external marketplace. The plugin uses the **talon-observability** binaries (talon-tap and talon-agent) that are included in this sandbox repository as a reference implementation.
>
> **Two components:**
> - **Sandbox (this repo)**: Contains the `talon-observability` example plugin with Rust binaries for trace collection
> - **External Marketplace**: Contains the `beak-tracer` plugin that uses the talon binaries to capture Claude Code events
>
> See the [Components Overview](#components-overview) section below for more details.

## Overview

The Beak Tracer plugin captures Claude Code observability traces and forwards them to your trace collection system. It hooks into Claude Code's event system to capture tool calls, session lifecycle, model usage, and context.

**What you get:**
- Complete visibility into Claude Code tool usage
- Token usage tracking and performance metrics
- Non-blocking hooks with < 10ms overhead
- Reliable delivery with automatic retry and spooling

## Components Overview

This setup involves two separate repositories:

### Sandbox Repository (This Repo)
**Location:** `$PROJECT_ROOT` (where you cloned the sandbox)

Contains:
- `talon-observability` plugin example
- Rust source code for `talon-tap` and `talon-agent` binaries
- Located at: `$PROJECT_ROOT/plugins/talon/`

### External Marketplace Repository
**Location:** `$MARKETPLACE_DIR` (where you cloned the marketplace)

Contains:
- `beak-tracer` plugin (the actual Claude Code plugin)
- Hook scripts that use the talon binaries
- Located at: `$MARKETPLACE_DIR/beak-tracer/`

## Architecture

```
Claude Code → forward-to-talon.sh → talon-tap → talon-agent → Your Trace Endpoint
```

**Components:**
1. **Claude Code Plugin** (beak-tracer) - Registers hooks to capture events (from marketplace)
2. **talon-tap** - Lightweight collector that receives events via stdin (from sandbox)
3. **talon-agent** - Background daemon for batching, enrichment, and HTTP delivery (from sandbox)

## Prerequisites

### 1. Rust Toolchain

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env
```

### 2. Trace Collection Endpoint

You need an HTTP endpoint that accepts POST requests. Options:
- Beak (local or remote)
- OpenTelemetry collector
- Custom trace service

**Quick test endpoint** (Python):

```python
# test-endpoint.py
from http.server import HTTPServer, BaseHTTPRequestHandler
import json, gzip

class TraceHandler(BaseHTTPRequestHandler):
    def do_POST(self):
        content_length = int(self.headers['Content-Length'])
        body = self.rfile.read(content_length)

        if self.headers.get('Content-Encoding') == 'gzip':
            body = gzip.decompress(body)

        traces = json.loads(body)
        print(f"Received {len(traces)} traces")

        self.send_response(200)
        self.send_header('Content-type', 'application/json')
        self.end_headers()
        self.wfile.write(b'{"status":"ok"}')

print("Test endpoint on http://localhost:3000/v1/traces")
HTTPServer(('localhost', 3000), TraceHandler).serve_forever()
```

Run: `python test-endpoint.py`

## Installation

### Step 1: Build Rust Binaries

```bash
cd $PROJECT_ROOT/plugins/talon
cargo build --release
```

This creates:
- `$PROJECT_ROOT/plugins/talon/target/release/talon-tap`
- `$PROJECT_ROOT/plugins/talon/target/release/talon-agent`

### Step 2: Add Binaries to PATH

Add to `~/.zshrc` or `~/.bashrc`:

```bash
# Add talon binaries to PATH
# Replace $PROJECT_ROOT with the full path to your sandbox clone
export PATH="$PROJECT_ROOT/plugins/talon/target/release:$PATH"
```

Apply changes:

```bash
source ~/.zshrc  # or source ~/.bashrc
```

Verify:

```bash
which talon-tap
which talon-agent
```

### Step 3: Install Claude Code Plugin

```bash
# Add marketplace to Claude Code
# Replace $MARKETPLACE_DIR with the full path to your marketplace clone
/plugin marketplace add $MARKETPLACE_DIR

# Install beak-tracer plugin
/plugin install beak-tracer@beak-plugins
```

### Step 4: Restart Claude Code

Restart your Claude Code session to activate the plugin.

### Step 5: Verify Installation

```bash
# List installed plugins - beak-tracer should appear
/plugin
```

## Configuration

### Required Environment Variables

Add to `~/.zshrc` or `~/.bashrc`:

```bash
# Trace endpoint URL (where to send traces)
export TRACE_ENDPOINT="http://localhost:3000/v1/traces"

# API key for authentication (optional, depends on your endpoint)
export TRACE_API_KEY="your-api-key-here"

# Add talon binaries to PATH (if not already added)
# Replace $PROJECT_ROOT with the full path to your sandbox clone
export PATH="$PROJECT_ROOT/plugins/talon/target/release:$PATH"
```

Apply:

```bash
source ~/.zshrc  # or source ~/.bashrc
```

### Environment Variables Reference

#### Required

| Variable | Description | Example |
|----------|-------------|---------|
| `TRACE_ENDPOINT` | HTTP endpoint to send traces | `http://localhost:3000/v1/traces` |
| `PATH` | Must include talon binaries | Add `talon/target/release` to PATH |

#### Optional

| Variable | Description | Default |
|----------|-------------|---------|
| `TRACE_API_KEY` | API key for authentication | None |
| `TALON_SOCK` | Unix socket path for IPC | `/tmp/talon.sock` |
| `TALON_TAP_PATH` | Explicit path to talon-tap binary | Uses `PATH` lookup |
| `DEBUG_HOOK` | Enable debug logging | `0` (disabled) |
| `TRACE_TIMEOUT_S` | HTTP request timeout in seconds | `8` |
| `TRACE_SAMPLE_RATE` | Sample rate (0.0-1.0) | `1.0` (capture all) |

#### Agent Configuration (Advanced)

| Flag | Description | Default |
|------|-------------|---------|
| `--batch-size` | Max events per batch | `100` |
| `--batch-ms` | Batch window in milliseconds | `200` |
| `--batch-bytes` | Max batch size in bytes | `1048576` (1MB) |
| `--spool-dir` | Directory for spooled events | Platform default |

## Quick Start / Testing

### Test 1: Verify Binaries

```bash
which talon-tap
which talon-agent
```

Expected output:
```
$PROJECT_ROOT/plugins/talon/target/release/talon-tap
$PROJECT_ROOT/plugins/talon/target/release/talon-agent
```

### Test 2: Manual Hook Test

```bash
cd $MARKETPLACE_DIR/beak-tracer
./test-hook.sh
```

### Test 3: Test talon-tap

```bash
# Send test event
echo '{"session_id":"test-123","tool_name":"Bash"}' | talon-tap --event PostToolUse

# Check agent started
ps aux | grep talon-agent
```

### Test 4: Test in Claude Code

In Claude Code, run any command (e.g., `/help`). Check your trace endpoint logs to verify traces arrive.

### Test 5: Check Spooled Events

If your endpoint is down, events spool to disk:

```bash
# macOS
cat ~/Library/Application\ Support/talon/spool/events.jsonl

# Linux
cat ~/.local/share/talon/spool/events.jsonl
```

Manually flush when endpoint is back:

```bash
talon-agent flush --endpoint "$TRACE_ENDPOINT" --api-key "$TRACE_API_KEY"
```

## Troubleshooting

### "talon-tap not found"

1. Verify binary exists:
   ```bash
   ls -la $PROJECT_ROOT/plugins/talon/target/release/talon-tap
   ```

2. Check PATH:
   ```bash
   echo $PATH | grep talon
   ```

3. Rebuild if missing:
   ```bash
   cd $PROJECT_ROOT/plugins/talon
   cargo build --release
   ```

4. Set explicit path:
   ```bash
   export TALON_TAP_PATH="$PROJECT_ROOT/plugins/talon/target/release/talon-tap"
   ```

### "Connection refused" to trace endpoint

1. Verify endpoint is running:
   ```bash
   curl -X POST "$TRACE_ENDPOINT" -H "Content-Type: application/json" -d '[{"test":"trace"}]'
   ```

2. Check endpoint URL:
   ```bash
   echo $TRACE_ENDPOINT
   ```

3. Manually flush spooled events when endpoint is back:
   ```bash
   talon-agent flush --endpoint "$TRACE_ENDPOINT" --api-key "$TRACE_API_KEY"
   ```

### Plugin not triggering

1. Verify plugin is installed:
   ```bash
   /plugin
   ```

2. Enable debug logging:
   ```bash
   export DEBUG_HOOK=1
   tail -f /tmp/beak-tracer-debug.log
   ```

3. Restart Claude Code after installing plugin

### High memory usage

1. Check spool file size:
   ```bash
   ls -lh ~/Library/Application\ Support/talon/spool/
   ```

2. Reduce batch size:
   ```bash
   talon-agent start --endpoint "$TRACE_ENDPOINT" --api-key "$TRACE_API_KEY" --batch-size 50
   ```

3. Flush and clear spool:
   ```bash
   talon-agent flush --endpoint "$TRACE_ENDPOINT" --api-key "$TRACE_API_KEY"
   rm ~/Library/Application\ Support/talon/spool/events.jsonl
   ```

### "Permission denied" on Unix socket

1. Check socket permissions:
   ```bash
   ls -la /tmp/talon.sock
   ```

2. Remove stale socket:
   ```bash
   rm /tmp/talon.sock
   ```

3. Restart agent:
   ```bash
   pkill talon-agent
   # Agent will auto-restart on next event
   ```

## Quick Start Checklist

**Part 1: Build Talon Binaries**
- [ ] Install Rust toolchain
- [ ] Build talon binaries (`cargo build --release`)
- [ ] Add binaries to PATH

**Part 2: Configure Environment**
- [ ] Set `TRACE_ENDPOINT` environment variable
- [ ] Set `TRACE_API_KEY` environment variable (optional)
- [ ] Source shell profile (`source ~/.zshrc`)
- [ ] Verify binaries accessible (`which talon-tap && which talon-agent`)

**Part 3: Install Claude Code Plugin**
- [ ] Add marketplace (`/plugin marketplace add $MARKETPLACE_DIR`)
- [ ] Install plugin (`/plugin install beak-tracer@beak-plugins`)
- [ ] Restart Claude Code
- [ ] Verify installation (`/plugin`)

**Part 4: Test & Verify**
- [ ] Run test script (`./test-hook.sh`)
- [ ] Use Claude Code (run any command)
- [ ] Check traces arriving at endpoint

## Advanced Configuration

### Running Agent in Foreground (Debugging)

```bash
talon-agent start \
  --endpoint "http://localhost:3000/v1/traces" \
  --api-key "your-key" \
  --sock "/tmp/talon.sock"
```

### Custom Spool Directory

```bash
export TRACE_SPOOL_DIR="/custom/path/to/spool"

talon-agent start \
  --endpoint "$TRACE_ENDPOINT" \
  --api-key "$TRACE_API_KEY" \
  --spool-dir "$TRACE_SPOOL_DIR"
```

### Multiple Endpoints

Run multiple agent instances:

```bash
# Agent 1: Primary endpoint
talon-agent start \
  --sock "/tmp/talon-primary.sock" \
  --endpoint "http://primary.example.com/v1/traces"

# Agent 2: Backup endpoint
talon-agent start \
  --sock "/tmp/talon-backup.sock" \
  --endpoint "http://backup.example.com/v1/traces"
```

## Files Reference

### Plugin Files (Marketplace)

| Path | Description |
|------|-------------|
| `$MARKETPLACE_DIR/beak-tracer/hooks.json` | Hook definitions |
| `$MARKETPLACE_DIR/beak-tracer/hooks/forward-to-talon.sh` | Main hook script |

### Binary Files (Sandbox)

| Path | Description |
|------|-------------|
| `$PROJECT_ROOT/plugins/talon/target/release/talon-tap` | Event collector |
| `$PROJECT_ROOT/plugins/talon/target/release/talon-agent` | Event processor |

### Runtime Files

| Path | Description |
|------|-------------|
| `/tmp/talon.sock` | Unix socket for IPC |
| `~/Library/Application Support/talon/spool/events.jsonl` | Spooled events (macOS) |
| `~/.local/share/talon/spool/events.jsonl` | Spooled events (Linux) |
| `/tmp/beak-tracer-debug.log` | Debug log (when `DEBUG_HOOK=1`) |

## Performance

- **Hook execution time**: < 15ms per tool call
- **Batching efficiency**: 200ms windows reduce HTTP overhead by 90%+
- **Compression**: 5-10x size reduction with gzip
- **Process isolation**: Agent crashes don't affect Claude Code

## Support

For issues or questions:
1. Check the [Troubleshooting](#troubleshooting) section
2. Enable debug logging: `export DEBUG_HOOK=1`
3. Check spool directory for quarantined events
4. Review [Claude Code Plugin docs](https://code.claude.com/docs/en/plugins)

## License

MIT OR Apache-2.0
