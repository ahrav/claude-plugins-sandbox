# Claude Code Beak Tracer Plugin - Complete Setup Guide

## Table of Contents
- [Overview](#overview)
- [How It Works](#how-it-works)
- [Understanding the Plugin Structure](#understanding-the-plugin-structure)
- [Architecture](#architecture)
- [Prerequisites](#prerequisites)
- [Installation](#installation)
- [Configuration](#configuration)
- [Setting Up Your Trace Endpoint](#setting-up-your-trace-endpoint)
- [Testing](#testing)
- [Troubleshooting](#troubleshooting)
- [Advanced Configuration](#advanced-configuration)

---

## Overview

The **Beak Tracer** plugin captures observability traces from Claude Code sessions and forwards them to your trace collection system (e.g., Beak). It hooks into Claude Code's event system to capture:

- **Tool calls** (Bash, Read, Write, Edit, etc.) - captured in real-time via `PostToolUse` hook
- **Session lifecycle** (conversation start/end) - captured via `Stop` hook
- **Model usage** (token counts, model info, latency)
- **Context** (working directory, session ID, environment)

All traces are enriched with transcript data and sent to your configured trace endpoint with automatic batching, retries, and spooling for reliability.

### What You Get

✅ Complete visibility into Claude Code tool usage
✅ Token usage tracking across sessions
✅ Performance metrics (latency, throughput)
✅ Non-blocking hooks (< 10ms overhead)
✅ Reliable delivery with automatic retry and spooling
✅ Production-ready Rust binaries

### What This Setup Involves

This system has **two main components**:

1. **Claude Code Plugin** (beak-tracer)
   - Location: `/Users/ahrav/Projects/claude-plugins-marketplace/beak-tracer/`
   - Contains: hooks.json, forward-to-talon.sh script
   - Purpose: Registers hooks in Claude Code to capture events

2. **Talon Binaries** (talon-tap & talon-agent)
   - Location: `/Users/ahrav/Projects/claude-plugins-sandbox/plugins/talon/`
   - Contains: Rust binaries for event collection and processing
   - Purpose: Handle batching, enrichment, and HTTP delivery

**The plugin calls the binaries**: When Claude Code fires a hook, the plugin's `forward-to-talon.sh` script pipes the event to `talon-tap`, which forwards to `talon-agent`, which sends to your trace endpoint.

---

## How It Works

```
┌─────────────────────────────────────────────────────────────┐
│                      Claude Code                            │
│  User interacts → Tools execute → Hooks fire                │
└────────────────────────┬────────────────────────────────────┘
                         │ stdin (JSON payload)
                         ▼
┌─────────────────────────────────────────────────────────────┐
│              forward-to-talon.sh (Hook Script)              │
│  Receives event from Claude Code, pipes to talon-tap        │
└────────────────────────┬────────────────────────────────────┘
                         │ stdin pipe
                         ▼
┌─────────────────────────────────────────────────────────────┐
│                   talon-tap (Collector)                     │
│  • Receives events via stdin                                │
│  • Wraps in envelope with metadata                          │
│  • Sends to talon-agent via Unix socket                     │
│  • Auto-starts agent if not running                         │
└────────────────────────┬────────────────────────────────────┘
                         │ Unix socket (/tmp/talon.sock)
                         ▼
┌─────────────────────────────────────────────────────────────┐
│                 talon-agent (Processor)                     │
│  • Receives events from talon-tap                           │
│  • Batches events (200ms window)                            │
│  • Enriches with transcript data                            │
│  • Compresses with gzip                                     │
│  • Sends to trace endpoint via HTTP POST                    │
│  • Handles retries with exponential backoff                 │
│  • Spools failed requests to disk                           │
└────────────────────────┬────────────────────────────────────┘
                         │ HTTP POST (gzipped JSON)
                         ▼
┌─────────────────────────────────────────────────────────────┐
│              Your Trace Endpoint                            │
│  Example: http://localhost:3000/v1/traces                   │
│  Receives batched, enriched trace events                    │
└─────────────────────────────────────────────────────────────┘
```

### Why This Architecture?

1. **Performance**: Hook scripts execute instantly (just pipe to talon-tap)
2. **Reliability**: talon-agent handles retries and disk spooling
3. **Off Critical Path**: Transcript enrichment happens in agent's batching window
4. **Process Isolation**: Agent crashes don't affect Claude Code
5. **Batching**: Reduces HTTP overhead by 90%+ (200ms batching window)

---

## Understanding the Plugin Structure

Before diving into installation, let's understand what the "plugin" actually is and how it's structured.

### What is the Beak Tracer Plugin?

The **beak-tracer plugin** is a directory located at:
```
/Users/ahrav/Projects/claude-plugins-marketplace/beak-tracer/
```

This directory contains everything Claude Code needs to hook into your sessions and forward traces.

### Plugin Directory Structure

```
beak-tracer/                              # ← The Claude Code Plugin
├── .claude-plugin/
│   └── plugin.json                       # Plugin metadata (name, version, author)
├── hooks.json                            # Hooks configuration (WHAT events to capture)
├── hooks/
│   └── forward-to-talon.sh               # Hook script (HOW to handle events)
├── setup-talon.sh                        # Setup helper script
├── test-hook.sh                          # Test script
└── README.md                             # Plugin documentation
```

### Three Critical Files

#### 1. `.claude-plugin/plugin.json` - Plugin Manifest

This tells Claude Code about your plugin:

```json
{
  "name": "beak-tracer",
  "version": "2.0.0",
  "description": "Captures Claude Code tool calls and stop events, forwarding them to Talon agent for Beak trace collection",
  "author": {
    "name": "Beak Team"
  },
  "hooks": "./hooks.json"
}
```

**Key field**: `"hooks": "./hooks.json"` tells Claude Code where to find the hooks configuration.

#### 2. `hooks.json` - Hooks Configuration

This defines WHICH Claude Code events to capture:

```json
{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "*",
        "hooks": [
          {
            "type": "command",
            "command": "${CLAUDE_PLUGIN_ROOT}/hooks/forward-to-talon.sh PostToolUse"
          }
        ]
      }
    ],
    "Stop": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "${CLAUDE_PLUGIN_ROOT}/hooks/forward-to-talon.sh Stop"
          }
        ]
      }
    ]
  }
}
```

**What this means**:
- **PostToolUse hook**: Fires after EVERY tool execution (Bash, Read, Write, Edit, etc.)
  - `"matcher": "*"` means capture ALL tools
  - Runs `forward-to-talon.sh PostToolUse` for each event
- **Stop hook**: Fires when a conversation/session ends
  - Runs `forward-to-talon.sh Stop`

**Important**: `${CLAUDE_PLUGIN_ROOT}` is automatically replaced by Claude Code with the plugin's installation path.

#### 3. `hooks/forward-to-talon.sh` - Hook Script

This is the bridge between Claude Code and the talon system:

```bash
#!/usr/bin/env bash
EVENT_TYPE="${1:-unknown}"

# Find talon-tap binary
if [ -n "${TALON_TAP_PATH:-}" ]; then
    TALON_TAP="$TALON_TAP_PATH"
elif [ -f "/Users/ahrav/Projects/claude-plugins-sandbox/plugins/talon/target/release/talon-tap" ]; then
    TALON_TAP="/Users/ahrav/Projects/claude-plugins-sandbox/plugins/talon/target/release/talon-tap"
elif command -v talon-tap &> /dev/null; then
    TALON_TAP="talon-tap"
else
    echo "Error: talon-tap not found" >&2
    exit 1
fi

# Read hook payload from stdin
STDIN_DATA=$(cat)

# Forward to talon-tap
echo "$STDIN_DATA" | "$TALON_TAP" --event "$EVENT_TYPE"
```

**What this does**:
1. Receives event type as argument (`PostToolUse` or `Stop`)
2. Locates the `talon-tap` binary
3. Reads the event payload from stdin (JSON)
4. Pipes the payload to `talon-tap`

### The Marketplace Structure

The plugin lives inside a marketplace:

```
claude-plugins-marketplace/               # ← The Marketplace
├── .claude-plugin/
│   └── marketplace.json                  # Marketplace manifest
├── beak-tracer/                          # ← The Plugin
│   ├── .claude-plugin/plugin.json
│   ├── hooks.json
│   └── hooks/forward-to-talon.sh
└── README.md
```

**Marketplace manifest** (`marketplace.json`):

```json
{
  "name": "beak-plugins",
  "description": "Plugins for Beak trace collection and analysis",
  "owner": {
    "name": "Your Team"
  },
  "plugins": [
    {
      "name": "beak-tracer",
      "source": "./beak-tracer",
      "description": "Captures Claude Code tool calls and stop events..."
    }
  ]
}
```

This tells Claude Code:
- The marketplace is called `beak-plugins`
- It contains one plugin: `beak-tracer`
- The plugin's files are in `./beak-tracer/`

### How It All Connects

```
1. You add marketplace to Claude Code:
   /plugin marketplace add /path/to/claude-plugins-marketplace

2. Claude Code reads marketplace.json:
   → Discovers "beak-tracer" plugin at ./beak-tracer/

3. You install the plugin:
   /plugin install beak-tracer@beak-plugins

4. Claude Code copies beak-tracer/ to its plugins directory and reads:
   → .claude-plugin/plugin.json (metadata)
   → hooks.json (event configuration)

5. When you use Claude Code:
   → Tool executes (e.g., Bash)
   → PostToolUse hook fires
   → Claude Code runs: forward-to-talon.sh PostToolUse
   → Script pipes event to talon-tap
   → talon-tap sends to talon-agent
   → talon-agent sends to your trace endpoint
```

### Summary

- **Plugin**: The `beak-tracer/` directory with plugin.json, hooks.json, and hook scripts
- **Marketplace**: The `claude-plugins-marketplace/` directory that contains plugins
- **Hooks**: The configuration in `hooks.json` that defines WHICH events to capture
- **Hook Script**: The `forward-to-talon.sh` that bridges Claude Code to talon binaries
- **Talon Binaries**: The `talon-tap` and `talon-agent` that handle trace collection

---

## Architecture

### 2-Part System

#### 1. **talon-tap** (Event Collector)
**Location**: `/Users/ahrav/Projects/claude-plugins-sandbox/plugins/talon/target/release/talon-tap`

**Purpose**: Ultra-lightweight collector that receives hook events and forwards to agent

**Features**:
- Reads JSON from stdin
- Wraps events with metadata (timestamp, environment, session)
- Forwards via Unix socket (low latency)
- Auto-starts agent if not running
- Falls back to TCP on Windows

**Usage**:
```bash
echo '{"session_id":"abc","tool_name":"Bash"}' | talon-tap --event PostToolUse
```

#### 2. **talon-agent** (Event Processor)
**Location**: `/Users/ahrav/Projects/claude-plugins-sandbox/plugins/talon/target/release/talon-agent`

**Purpose**: Background daemon for batching, enrichment, and HTTP delivery

**Features**:
- Listens on Unix socket (`/tmp/talon.sock`)
- Batches events (200ms window, 100 events, or 1MB)
- Enriches with transcript data (model, usage, timestamps)
- Compresses with gzip (5-10× size reduction)
- HTTP POST with retry (exponential backoff + jitter)
- Disk spooling for failed requests
- Manual flush command for offline traces

**Commands**:
```bash
# Start agent daemon
talon-agent start --endpoint "http://localhost:3000/v1/traces" --api-key "your-key"

# Manually flush spooled events
talon-agent flush --endpoint "http://localhost:3000/v1/traces" --api-key "your-key"
```

---

## Prerequisites

### 1. Rust Toolchain
The plugin binaries are written in Rust. Install Rust if you don't have it:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env
```

### 2. Trace Collection Endpoint
You need a trace collection service running that accepts HTTP POST requests. This could be:
- Beak (running locally or remote)
- OpenTelemetry collector
- Custom trace service
- See [Setting Up Your Trace Endpoint](#setting-up-your-trace-endpoint)

---

## Installation

### Step 1: Build the Rust Binaries

```bash
cd /Users/ahrav/Projects/claude-plugins-sandbox/plugins/talon
cargo build --release
```

This creates two binaries:
- `target/release/talon-tap`
- `target/release/talon-agent`

### Step 2: Add Binaries to PATH

Add the following to your shell profile (`~/.zshrc` or `~/.bashrc`):

```bash
# Add talon binaries to PATH
export PATH="/Users/ahrav/Projects/claude-plugins-sandbox/plugins/talon/target/release:$PATH"
```

Apply changes:
```bash
source ~/.zshrc  # or source ~/.bashrc
```

Verify installation:
```bash
which talon-tap
which talon-agent
```

### Step 3: Install the Claude Code Plugin

Now we install the **beak-tracer plugin** (the directory containing hooks.json and the hook script) from the marketplace.

```bash
# Add the marketplace to Claude Code
# This tells Claude Code about the claude-plugins-marketplace directory
/plugin marketplace add /Users/ahrav/Projects/claude-plugins-marketplace

# Install the beak-tracer plugin from the marketplace
# This copies the beak-tracer/ directory and registers its hooks
/plugin install beak-tracer@beak-plugins
```

**What happens**:
1. Claude Code reads `/Users/ahrav/Projects/claude-plugins-marketplace/.claude-plugin/marketplace.json`
2. Discovers the `beak-tracer` plugin at `./beak-tracer/`
3. Copies the plugin to Claude Code's plugin directory
4. Reads `beak-tracer/.claude-plugin/plugin.json` and `beak-tracer/hooks.json`
5. Registers the `PostToolUse` and `Stop` hooks
6. The hooks will now fire and execute `forward-to-talon.sh` whenever tools are used

### Step 4: Restart Claude Code

Some plugin installations require restarting Claude Code to activate. Restart your Claude Code session.

### Step 5: Verify Plugin Installation

Check that the plugin is installed and hooks are registered:

```bash
# List installed plugins - beak-tracer should appear
/plugin

# Optional: Check what hooks are active
# The plugin's hooks should be registered for PostToolUse and Stop events
```

You should see `beak-tracer` in the list of installed plugins.

---

## Configuration

### Required Environment Variables

Add these to your shell profile (`~/.zshrc` or `~/.bashrc`):

```bash
# ============================================================================
# Beak Tracer Plugin Configuration
# ============================================================================

# REQUIRED: Trace endpoint URL (where to send traces)
export TRACE_ENDPOINT="http://localhost:3000/v1/traces"

# OPTIONAL: API key for authentication (depends on your endpoint)
export TRACE_API_KEY="your-api-key-here"

# REQUIRED: Add talon binaries to PATH
export PATH="/Users/ahrav/Projects/claude-plugins-sandbox/plugins/talon/target/release:$PATH"
```

Apply changes:
```bash
source ~/.zshrc  # or source ~/.bashrc
```

### Environment Variables Reference

#### Required

| Variable | Description | Example |
|----------|-------------|---------|
| `TRACE_ENDPOINT` | HTTP endpoint to send traces | `http://localhost:3000/v1/traces` |
| `PATH` | Must include talon binaries | Add talon/target/release to PATH |

#### Optional

| Variable | Description | Default |
|----------|-------------|---------|
| `TRACE_API_KEY` | API key for trace endpoint authentication | None |
| `TALON_SOCK` | Unix socket path for IPC | `/tmp/talon.sock` |
| `TALON_TAP_PATH` | Explicit path to talon-tap binary | Uses `PATH` lookup |
| `TALON_AGENT_PATH` | Explicit path to talon-agent binary | Uses `PATH` lookup |
| `DEBUG_HOOK` | Enable debug logging to `/tmp/beak-tracer-debug.log` | `0` (disabled) |
| `TRACE_TIMEOUT_S` | HTTP request timeout in seconds | `8` |
| `TRACE_SAMPLE_RATE` | Sample rate (0.0-1.0) | `1.0` (capture everything) |
| `TALON_TAP_MAX_STDIN_BYTES` | Max stdin bytes talon-tap will read | `2097152` (2MB) |

#### Agent Configuration (Advanced)

These are passed to `talon-agent start`:

| Flag | Description | Default |
|------|-------------|---------|
| `--batch-size` | Max events per batch | `100` |
| `--batch-ms` | Batch window in milliseconds | `200` |
| `--batch-bytes` | Max batch size in bytes | `1048576` (1MB) |
| `--chan-capacity` | Internal channel capacity | `10000` |
| `--spool-bytes` | Max spool file size before rotation | `50000000` (50MB) |
| `--spool-dir` | Directory for spooled events | Platform default |

### Claude Code Automatic Environment Variables

Claude Code automatically sets these environment variables for your hooks:

| Variable | Description | Example |
|----------|-------------|---------|
| `CLAUDE_SESSION_ID` | Unique session identifier | `uuid` |
| `CLAUDE_TRANSCRIPT_PATH` | Path to session transcript (JSONL) | `~/.claude/projects/.../session-id.jsonl` |
| `CLAUDE_CWD` | Current working directory | `/path/to/project` |
| `CLAUDE_MODEL` | Model being used | `claude-sonnet-4-5-20250929` |
| `CLAUDE_PLATFORM` | Platform identifier | `claude-code` |
| `CLAUDE_PLUGIN_ROOT` | Plugin installation directory | `/path/to/beak-tracer` |
| `CLAUDE_PERMISSION_MODE` | Permission mode | `ask`, `allow`, etc. |
| `CLAUDE_IS_GIT_REPO` | Whether CWD is a git repo | `true` or `false` |

---

## Setting Up Your Trace Endpoint

### Option 1: Run Beak Locally (Recommended for Testing)

If you have Beak running locally:

```bash
# Start Beak on localhost:3000
# (Follow Beak's setup instructions)

# Configure the plugin
export TRACE_ENDPOINT="http://localhost:3000/v1/traces"
export TRACE_API_KEY="your-beak-api-key"
```

### Option 2: Mock Endpoint for Testing

Create a simple test endpoint using Python:

```python
# test-endpoint.py
from http.server import HTTPServer, BaseHTTPRequestHandler
import json
import gzip

class TraceHandler(BaseHTTPRequestHandler):
    def do_POST(self):
        content_length = int(self.headers['Content-Length'])
        body = self.rfile.read(content_length)

        # Decompress if gzipped
        if self.headers.get('Content-Encoding') == 'gzip':
            body = gzip.decompress(body)

        traces = json.loads(body)
        print(f"Received {len(traces)} traces:")
        for trace in traces:
            print(f"  - Event: {trace.get('event')}, Tool: {trace.get('inputs', {}).get('tool', {}).get('name')}")

        self.send_response(200)
        self.send_header('Content-type', 'application/json')
        self.end_headers()
        self.wfile.write(b'{"status":"ok"}')

    def log_message(self, format, *args):
        pass  # Suppress default logging

print("Starting test trace endpoint on http://localhost:3000/v1/traces")
HTTPServer(('localhost', 3000), TraceHandler).serve_forever()
```

Run it:
```bash
python test-endpoint.py

# In another terminal:
export TRACE_ENDPOINT="http://localhost:3000/v1/traces"
```

### Option 3: Remote Endpoint

For production deployments:

```bash
export TRACE_ENDPOINT="https://traces.your-domain.com/v1/traces"
export TRACE_API_KEY="prod-api-key-here"
```

---

## Testing

### Test 1: Verify Binaries Are Accessible

```bash
which talon-tap
which talon-agent
```

Expected output:
```
/Users/ahrav/Projects/claude-plugins-sandbox/plugins/talon/target/release/talon-tap
/Users/ahrav/Projects/claude-plugins-sandbox/plugins/talon/target/release/talon-agent
```

### Test 2: Manual Hook Test

Run the provided test script:

```bash
cd /Users/ahrav/Projects/claude-plugins-marketplace/beak-tracer
./test-hook.sh
```

This simulates a hook event and verifies the pipeline works.

### Test 3: Test talon-tap

```bash
# Manually send an event to talon-tap
echo '{"session_id":"test-123","tool_name":"Bash","cwd":"/tmp"}' | talon-tap --event PostToolUse

# Check agent received it (should auto-start)
ps aux | grep talon-agent
```

### Test 4: Test in Claude Code

Start a Claude Code session and run a simple command:

```bash
# In Claude Code, type:
/help
```

This will trigger hook events. Check your trace endpoint logs to verify traces are arriving.

### Test 5: Check Spooled Events

If your trace endpoint is down, events spool to disk:

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

---

## Troubleshooting

### Issue: "talon-tap not found"

**Symptom**: Hook fails with error `Error: talon-tap not found`

**Solution**:
1. Verify binary exists:
   ```bash
   ls -la /Users/ahrav/Projects/claude-plugins-sandbox/plugins/talon/target/release/talon-tap
   ```

2. Check PATH:
   ```bash
   echo $PATH | grep talon
   ```

3. Rebuild if missing:
   ```bash
   cd /Users/ahrav/Projects/claude-plugins-sandbox/plugins/talon
   cargo build --release
   ```

4. Set explicit path:
   ```bash
   export TALON_TAP_PATH="/full/path/to/talon-tap"
   ```

### Issue: "Connection refused" to trace endpoint

**Symptom**: Events spool to disk instead of sending

**Solution**:
1. Verify endpoint is running:
   ```bash
   curl -X POST "$TRACE_ENDPOINT" \
     -H "Content-Type: application/json" \
     -d '[{"test":"trace"}]'
   ```

2. Check endpoint URL:
   ```bash
   echo $TRACE_ENDPOINT
   ```

3. Review agent logs (if running in foreground):
   ```bash
   talon-agent start --endpoint "$TRACE_ENDPOINT" --api-key "$TRACE_API_KEY"
   ```

4. Manually flush spooled events when endpoint is back:
   ```bash
   talon-agent flush --endpoint "$TRACE_ENDPOINT" --api-key "$TRACE_API_KEY"
   ```

### Issue: Plugin not triggering

**Symptom**: No traces arriving at endpoint

**Solution**:
1. Verify plugin is installed:
   ```bash
   /plugin
   ```

2. Check hooks configuration:
   ```bash
   cat /Users/ahrav/Projects/claude-plugins-marketplace/beak-tracer/hooks.json
   ```

3. Enable debug logging:
   ```bash
   export DEBUG_HOOK=1
   tail -f /tmp/beak-tracer-debug.log
   ```

4. Restart Claude Code after installing plugin

### Issue: High memory usage

**Symptom**: talon-agent consuming lots of memory

**Solution**:
1. Check spool file size:
   ```bash
   ls -lh ~/Library/Application\ Support/talon/spool/
   ```

2. Reduce batch size:
   ```bash
   talon-agent start \
     --endpoint "$TRACE_ENDPOINT" \
     --api-key "$TRACE_API_KEY" \
     --batch-size 50 \
     --chan-capacity 5000
   ```

3. Manually flush and clear spool:
   ```bash
   talon-agent flush --endpoint "$TRACE_ENDPOINT" --api-key "$TRACE_API_KEY"
   rm ~/Library/Application\ Support/talon/spool/events.jsonl
   ```

### Issue: "Permission denied" on Unix socket

**Symptom**: talon-tap cannot connect to `/tmp/talon.sock`

**Solution**:
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

### Issue: Traces missing token counts

**Symptom**: `usage` field is empty or zero

**Solution**:
1. This is normal for some events (Stop hook has no usage data)
2. Token counts come from transcript enrichment
3. Verify transcript path is accessible:
   ```bash
   # Check a recent transcript
   ls -la ~/.claude/projects/
   ```

4. Token counts appear on `PostToolUse` events from assistant messages

---

## Advanced Configuration

### Running Agent in Foreground (Debugging)

```bash
# Start agent in foreground to see logs
talon-agent start \
  --endpoint "http://localhost:3000/v1/traces" \
  --api-key "your-key" \
  --sock "/tmp/talon.sock"
```

### Custom Spool Directory

```bash
# Use custom spool location
export TRACE_SPOOL_DIR="/custom/path/to/spool"

talon-agent start \
  --endpoint "$TRACE_ENDPOINT" \
  --api-key "$TRACE_API_KEY" \
  --spool-dir "$TRACE_SPOOL_DIR"
```

### Compression Control

The agent uses gzip compression by default. To disable (not recommended):

Modify the agent source code in `talon-agent/main.rs` to skip compression, or configure your endpoint to accept uncompressed payloads.

### Sampling

To reduce trace volume, sample events:

```bash
export TRACE_SAMPLE_RATE=0.1  # Capture 10% of events
```

Note: Sampling is not currently implemented in talon-agent but can be added to the codebase.

### Multiple Endpoints

To send traces to multiple endpoints, run multiple agent instances:

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

Update hook script to send to both:
```bash
# In forward-to-talon.sh
echo "$STDIN_DATA" | talon-tap --sock /tmp/talon-primary.sock --event "$EVENT_TYPE"
echo "$STDIN_DATA" | talon-tap --sock /tmp/talon-backup.sock --event "$EVENT_TYPE"
```

---

## Trace Payload Schema

Traces are sent as JSON arrays in the **TraceV1** schema:

```json
{
  "schema_version": "beak.trace.v1",
  "event": "tool.post",
  "timestamp": "2025-11-14T05:12:50.346Z",

  "ids": {
    "trace_id": "uuid",
    "span_id": "uuid",
    "parent_span_id": "",
    "conversation_id": "session-123",
    "session_id": "session-123"
  },

  "context": {
    "plugin": "talon",
    "plugin_version": "0.1.0",
    "host": "machine-name",
    "pid": 12345,
    "locale": "",
    "timezone": ""
  },

  "configuration": {
    "model": "claude-sonnet-4-5-20250929",
    "temperature": 0.0,
    "top_p": 0.0,
    "top_k": 0,
    "max_tokens": 0,
    "seed": 0,
    "stop_sequences": []
  },

  "inputs": {
    "messages_compact": [],
    "tool": {
      "name": "Bash",
      "version": "",
      "args": {
        "command": "ls -la"
      }
    },
    "retrieval_items": []
  },

  "outputs": {
    "assistant_text": "",
    "tool_calls": [],
    "finish_reason": "tool_use",
    "truncated": false
  },

  "metrics": {
    "prompt_tokens": 6734,
    "completion_tokens": 150,
    "total_tokens": 6884,
    "token_counts_estimated": false,
    "latency_ms": {
      "first_token": 0,
      "provider": 0,
      "total": 0
    },
    "latency_estimated": false,
    "input_cost_usd": 0.0,
    "output_cost_usd": 0.0,
    "total_cost_usd": 0.0,
    "quality_score": 0.0
  },

  "labels": [
    {
      "key": "host",
      "value": "machine-name"
    },
    {
      "key": "tool_name",
      "value": "Bash"
    }
  ],

  "flags": {
    "sampled": false
  },

  "extensions": {
    "tap.raw": {
      "cwd": "/path/to/project",
      "platform": "claude-code"
    }
  }
}
```

### Event Types

| Event | Description | Frequency |
|-------|-------------|-----------|
| `tool.post` | After tool execution (Bash, Read, Edit, etc.) | High (100+ per session) |
| `model.end` | When conversation ends | Low (once per session) |

---

## Performance Considerations

### Hook Execution Time

- **talon-tap**: < 10ms (pipe to Unix socket)
- **Hook script**: < 5ms (bash script overhead)
- **Total overhead**: < 15ms per tool call

### Batching Efficiency

- **Without batching**: 100 tool calls = 100 HTTP requests
- **With batching**: 100 tool calls = ~5 HTTP requests (200ms windows)
- **Compression**: 5-10× size reduction with gzip

### Transcript Reading Optimization

Long sessions (1000+ lines) use optimized transcript reading:
- Reads from end of file (where latest data is)
- Caches enriched data per session (5s TTL)
- Only re-reads if file modification time changed
- Result: 50× speedup vs naive full-file reading

---

## Files Reference

### Plugin Files

| Path | Description |
|------|-------------|
| `/Users/ahrav/Projects/claude-plugins-marketplace/.claude-plugin/marketplace.json` | Marketplace manifest |
| `/Users/ahrav/Projects/claude-plugins-marketplace/beak-tracer/.claude-plugin/plugin.json` | Plugin manifest |
| `/Users/ahrav/Projects/claude-plugins-marketplace/beak-tracer/hooks.json` | Hook definitions |
| `/Users/ahrav/Projects/claude-plugins-marketplace/beak-tracer/hooks/forward-to-talon.sh` | Main hook script |
| `/Users/ahrav/Projects/claude-plugins-marketplace/beak-tracer/test-hook.sh` | Test script |
| `/Users/ahrav/Projects/claude-plugins-marketplace/beak-tracer/setup-talon.sh` | Setup helper |

### Binary Files

| Path | Description |
|------|-------------|
| `/Users/ahrav/Projects/claude-plugins-sandbox/plugins/talon/target/release/talon-tap` | Event collector binary |
| `/Users/ahrav/Projects/claude-plugins-sandbox/plugins/talon/target/release/talon-agent` | Event processor binary |

### Source Files

| Path | Description |
|------|-------------|
| `/Users/ahrav/Projects/claude-plugins-sandbox/plugins/talon/src/bin/talon-tap.rs` | talon-tap source |
| `/Users/ahrav/Projects/claude-plugins-sandbox/plugins/talon/src/bin/talon-agent/main.rs` | talon-agent main source |
| `/Users/ahrav/Projects/claude-plugins-sandbox/plugins/talon/src/bin/talon-agent/map.rs` | Event mapping logic |
| `/Users/ahrav/Projects/claude-plugins-sandbox/plugins/talon/src/bin/talon-agent/schema.rs` | TraceV1 schema |

### Runtime Files

| Path | Description |
|------|-------------|
| `/tmp/talon.sock` | Unix socket for IPC |
| `~/Library/Application Support/talon/spool/events.jsonl` | Spooled events (macOS) |
| `~/.local/share/talon/spool/events.jsonl` | Spooled events (Linux) |
| `~/Library/Application Support/talon/spool/quarantine.jsonl` | Malformed events (macOS) |
| `/tmp/beak-tracer-debug.log` | Debug log (when `DEBUG_HOOK=1`) |
| `~/.claude/projects/<project-id>/<session-id>.jsonl` | Claude Code transcript |

---

## Quick Start Checklist

**Part 1: Build the Talon Binaries (talon-tap & talon-agent)**
- [ ] **Install Rust toolchain** (`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`)
- [ ] **Build talon binaries** (`cd /Users/ahrav/Projects/claude-plugins-sandbox/plugins/talon && cargo build --release`)
- [ ] **Add binaries to PATH** (add to `~/.zshrc` or `~/.bashrc`)

**Part 2: Configure Environment**
- [ ] **Set `TRACE_ENDPOINT`** environment variable (where traces are sent)
- [ ] **Set `TRACE_API_KEY`** environment variable (optional, depends on endpoint)
- [ ] **Source shell profile** (`source ~/.zshrc`)
- [ ] **Verify binaries accessible** (`which talon-tap && which talon-agent`)

**Part 3: Install the Claude Code Plugin (beak-tracer)**
- [ ] **Verify plugin exists** (check `/Users/ahrav/Projects/claude-plugins-marketplace/beak-tracer/` has hooks.json)
- [ ] **Add marketplace to Claude Code** (`/plugin marketplace add /Users/ahrav/Projects/claude-plugins-marketplace`)
- [ ] **Install beak-tracer plugin** (`/plugin install beak-tracer@beak-plugins`)
- [ ] **Restart Claude Code**
- [ ] **Verify plugin installed** (`/plugin` should show beak-tracer)

**Part 4: Test & Verify**
- [ ] **Run test script** (`cd /Users/ahrav/Projects/claude-plugins-marketplace/beak-tracer && ./test-hook.sh`)
- [ ] **Use Claude Code** (run any command to trigger hooks)
- [ ] **Check traces arriving at endpoint** (verify HTTP POST requests arriving)

---

## Support & Contributing

For issues, questions, or contributions:

1. Check the [Troubleshooting](#troubleshooting) section
2. Review [Claude Code Plugin docs](https://code.claude.com/docs/en/plugins)
3. Enable debug logging: `export DEBUG_HOOK=1`
4. Check spool directory for quarantined events
5. Review trace endpoint logs

---

## License

MIT OR Apache-2.0
