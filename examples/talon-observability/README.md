# Talon Observability Plugin Example

This example demonstrates how to capture observability traces from Claude Code sessions and forward them to your trace collection system. It hooks into Claude Code's event system to capture tool calls and session lifecycle events.

For complete architecture details and advanced configuration, see [../../docs/SETUP.md](../../docs/SETUP.md).

## What This Example Does

Captures and forwards:
- Tool calls (Bash, Read, Write, Edit, etc.) via `PostToolUse` hooks
- Session lifecycle (conversation start/end) via `Stop` hooks
- Enriched with transcript data (token counts, model info, latency)
- Automatic batching, retries, and spooling for reliability

## Quick Start

### 1. Build the Talon Binaries

```bash
cd ../../plugins/talon
cargo build --release
```

This creates two binaries:
- `target/release/talon-tap` - Event collector (receives hooks, pipes to agent)
- `target/release/talon-agent` - Event processor (batches, enriches, sends to endpoint)

### 2. Add Binaries to PATH

Add to your shell profile (`~/.zshrc` or `~/.bashrc`):

```bash
# Add talon binaries to PATH
export PATH="/Users/ahrav/Projects/claude-plugins-sandbox/plugins/talon/target/release:$PATH"
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

### 3. Configure Environment Variables

Add to your shell profile:

```bash
# Required: Trace endpoint URL
export TRACE_ENDPOINT="http://localhost:3000/v1/traces"

# Optional: API key for authentication (endpoint-dependent)
export TRACE_API_KEY="your-api-key-here"
```

Apply changes:
```bash
source ~/.zshrc  # or source ~/.bashrc
```

### 4. Install the Plugin

Copy this example to your project:

```bash
# Option 1: Copy to your project
cp -r examples/talon-observability /path/to/your/project/.claude-plugin

# Option 2: Symlink for development
ln -s /path/to/claude-plugins-sandbox/examples/talon-observability \
      /path/to/your/project/.claude-plugin
```

### 5. Test the Plugin

```bash
cd examples/talon-observability
./test-plugin.sh
```

This simulates a hook event and verifies the pipeline works.

## Plugin Structure

```
talon-observability/
├── .claude-plugin/
│   └── marketplace.json        # Plugin metadata
├── hooks/
│   ├── hooks.json              # Hook configuration (which events to capture)
│   └── forward-to-talon.sh     # Hook script (pipes events to talon-tap)
├── test-plugin.sh              # Test script
└── README.md                   # This file
```

### Key Files

**`.claude-plugin/marketplace.json`**
- Declares plugin metadata (name, version, description)
- References hooks configuration

**`hooks/hooks.json`**
- Defines which Claude Code events to capture
- `PostToolUse` with `*` matcher: captures ALL tool invocations
- `Stop`: captures when Claude finishes responding

**`hooks/forward-to-talon.sh`**
- Receives hook event JSON via stdin
- Pipes to `talon-tap` with the event type
- Handles errors and provides feedback

## How It Works

```
Claude Code → forward-to-talon.sh → talon-tap → talon-agent → Trace Endpoint
   (hook)        (pipe stdin)       (socket)     (HTTP POST)
```

1. **Claude Code fires hook** (e.g., after tool execution)
2. **Hook script runs** (`forward-to-talon.sh PostToolUse`)
3. **Script pipes JSON to talon-tap** via stdin
4. **talon-tap wraps event** with metadata, sends to talon-agent via Unix socket
5. **talon-agent batches events** (200ms window), enriches with transcript data
6. **talon-agent sends batch** to trace endpoint via HTTP POST (gzipped)

## Configuration

### Required Environment Variables

| Variable | Description | Example |
|----------|-------------|---------|
| `TRACE_ENDPOINT` | HTTP endpoint for traces | `http://localhost:3000/v1/traces` |
| `PATH` | Must include talon binaries | Add `talon/target/release` to PATH |

### Optional Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `TRACE_API_KEY` | API key for authentication | None |
| `TALON_SOCK` | Unix socket path | `/tmp/talon.sock` |
| `TALON_TAP_PATH` | Explicit path to talon-tap | Uses `PATH` lookup |
| `TALON_AGENT_PATH` | Explicit path to talon-agent | Uses `PATH` lookup |
| `TALON_TAP_MAX_STDIN_BYTES` | Max stdin bytes talon-tap reads | `2097152` (2MB) |

### Trace Endpoint Options

The plugin works with any endpoint that accepts JSON over HTTP/HTTPS:

- **Beak (local):** `http://localhost:3000/v1/traces`
- **Honeycomb:** `https://api.honeycomb.io/v1/traces`
- **Datadog:** `https://http-intake.logs.datadoghq.com/v1/input`
- **Custom:** Any HTTP/HTTPS endpoint accepting gzipped JSON

**Example: Mock Endpoint for Testing**

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
        self.end_headers()
        self.wfile.write(b'{"status":"ok"}')

HTTPServer(('localhost', 3000), TraceHandler).serve_forever()
```

Run: `python test-endpoint.py`

## Testing

### 1. Verify Binaries

```bash
which talon-tap
which talon-agent
```

### 2. Run Test Script

```bash
cd examples/talon-observability
./test-plugin.sh
```

### 3. Manual Event Test

```bash
echo '{"session_id":"test","tool_name":"Bash"}' | talon-tap --event PostToolUse
ps aux | grep talon-agent  # Should be running
```

### 4. Test in Claude Code

Use Claude Code with the plugin installed. Every tool execution will trigger hooks.

### 5. Check Trace Endpoint

Verify traces are arriving at your endpoint. If using the mock endpoint:
```bash
# Should see output like:
# Received 5 traces
```

## Monitoring

### Check Agent Status

```bash
# Check if agent is running
ps aux | grep talon-agent

# Check socket exists
ls -la /tmp/talon.sock
```

### View Spooled Events

If the trace endpoint is unreachable, events spool to disk:

```bash
# macOS
cat ~/Library/Application\ Support/talon/spool/events.jsonl

# Linux
cat ~/.local/share/talon/spool/events.jsonl
```

### Manually Flush Spooled Events

```bash
talon-agent flush \
  --endpoint "$TRACE_ENDPOINT" \
  --api-key "$TRACE_API_KEY"
```

### Check Quarantined Events

Events that fail to parse are quarantined:

```bash
# macOS
cat ~/Library/Application\ Support/talon/spool/quarantine.jsonl

# Linux
cat ~/.local/share/talon/spool/quarantine.jsonl
```

## Troubleshooting

### "talon-tap not found"

**Solution:**
```bash
# Check PATH
echo $PATH | grep talon

# Rebuild if missing
cd ../../plugins/talon && cargo build --release

# Or set explicit path
export TALON_TAP_PATH="/full/path/to/talon-tap"
```

### No Traces Arriving at Endpoint

**Check list:**
1. Verify endpoint is running:
   ```bash
   curl -X POST "$TRACE_ENDPOINT" -H "Content-Type: application/json" -d '[]'
   ```
2. Check agent is running:
   ```bash
   ps aux | grep talon-agent
   ```
3. Check spool directory for queued events:
   ```bash
   ls -lh ~/Library/Application\ Support/talon/spool/
   ```
4. Run agent in foreground to see logs:
   ```bash
   talon-agent start --endpoint "$TRACE_ENDPOINT" --api-key "$TRACE_API_KEY"
   ```

### Permission Denied on Hook Script

```bash
chmod +x hooks/forward-to-talon.sh
```

### Events are Batched (Expected Behavior)

The agent batches events for efficiency:
- **Batch size:** 100 events (configurable with `--batch-size`)
- **Batch time:** 200ms (configurable with `--batch-ms`)
- **Batch bytes:** 1MB (configurable with `--batch-bytes`)

Events send when any threshold is reached.

## Customization

### Add More Hook Events

Edit `hooks/hooks.json` to capture additional events:

```json
{
  "hooks": {
    "PostToolUse": [...],
    "Stop": [...],
    "SessionStart": [
      {
        "hooks": [{
          "type": "command",
          "command": "${CLAUDE_PLUGIN_ROOT}/hooks/forward-to-talon.sh SessionStart"
        }]
      }
    ]
  }
}
```

**Available hook events:**
- `PreToolUse` - Before tool execution
- `PostToolUse` - After tool execution
- `Stop` - When agent finishes responding
- `SubagentStop` - When subagent task completes
- `SessionStart` - Session begins
- `SessionEnd` - Session ends
- `UserPromptSubmit` - Before processing user input
- `Notification` - When Claude sends notifications
- `PreCompact` - Before context compaction

### Filter Specific Tools

Use matchers to target specific tools:

```json
{
  "PostToolUse": [
    {
      "matcher": "Read|Write|Edit",
      "hooks": [{
        "type": "command",
        "command": "${CLAUDE_PLUGIN_ROOT}/hooks/forward-to-talon.sh PostToolUse"
      }]
    }
  ]
}
```

## Captured Event Examples

### PostToolUse Event

```json
{
  "event": "PostToolUse",
  "payload": {
    "session_id": "abc123...",
    "tool_name": "Read",
    "tool_input": {"file_path": "/path/to/file.rs"},
    "tool_response": "file contents...",
    "cwd": "/path/to/project"
  },
  "ts": "2025-11-13T10:30:00Z"
}
```

### Stop Event

```json
{
  "event": "Stop",
  "payload": {
    "session_id": "abc123...",
    "transcript_path": "/path/to/transcript.json",
    "cwd": "/path/to/project"
  },
  "ts": "2025-11-13T10:30:05Z"
}
```

### Trace Schema (Sent to Endpoint)

The talon-agent transforms events into the canonical `beak.trace.v1` schema with enriched data:

- **IDs:** trace_id, span_id, conversation_id, session_id
- **Context:** plugin version, host, pid, timestamps
- **Configuration:** model, temperature, max_tokens
- **Inputs:** tool name, args, messages
- **Outputs:** assistant text, tool calls, finish reason
- **Metrics:** prompt_tokens, completion_tokens, latency_ms, costs
- **Labels:** host, tool_name, event type
- **Extensions:** cwd, platform, raw event data

See `../../plugins/talon/src/bin/talon-agent/schema.rs` for full schema.

## Performance

- **Hook overhead:** < 15ms per tool call (pipe to Unix socket)
- **Batching efficiency:** 100 tool calls = ~5 HTTP requests (vs 100 without batching)
- **Compression:** 5-10x size reduction with gzip
- **Transcript reading:** Optimized for long sessions (reads from end, caches results)

## Security

1. **API Keys:** Store `TRACE_API_KEY` in shell profile, not in repository
2. **Socket Permissions:** Unix socket created with 0o600 (owner read/write only)
3. **Input Validation:** Hook script uses `set -euo pipefail` for safer execution
4. **Rate Limiting:** Agent batches events to avoid overwhelming collector

## Reference

### File Locations

**Plugin Files:**
- Plugin manifest: `.claude-plugin/marketplace.json`
- Hooks config: `hooks/hooks.json`
- Hook script: `hooks/forward-to-talon.sh`

**Binary Files:**
- Event collector: `../../plugins/talon/target/release/talon-tap`
- Event processor: `../../plugins/talon/target/release/talon-agent`

**Runtime Files:**
- Unix socket: `/tmp/talon.sock`
- Spool (macOS): `~/Library/Application Support/talon/spool/events.jsonl`
- Spool (Linux): `~/.local/share/talon/spool/events.jsonl`
- Quarantine (macOS): `~/Library/Application Support/talon/spool/quarantine.jsonl`

### Related Documentation

- [Complete Setup Guide](../../docs/SETUP.md) - Full architecture and advanced config
- [Talon Plugin README](../../plugins/talon/README.md) - Binary implementation details
- [Schema Definition](../../plugins/talon/src/bin/talon-agent/schema.rs) - TraceV1 schema

## License

Licensed under either of

- [Apache License, Version 2.0](../../LICENSE-APACHE)
- [MIT License](../../LICENSE)

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in this project by you, as defined in the Apache
License, shall be dual licensed as above, without any additional terms or
conditions.
