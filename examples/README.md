# Claude Code Plugin Examples

Example plugins demonstrating how to integrate with Claude Code's plugin system.

## Available Examples

### `talon-observability/`
Complete observability plugin showing how to capture Claude Code events and forward them to trace collectors (Honeycomb, Datadog, etc.). Uses a tap + agent architecture for buffering and batching.

**See [`talon-observability/README.md`](./talon-observability/README.md) for full documentation.**

## Quick Start

Use examples as templates for your own plugins:

```bash
# Copy example plugin structure to your project
cp -r examples/talon-observability/.claude-plugin /path/to/your/project/

# Required plugin structure:
# your-project/
# └── .claude-plugin/
#     ├── marketplace.json    # Plugin manifest
#     └── hooks/
#         ├── hooks.json      # Hook configuration
#         └── *.sh            # Hook scripts
```

## Available Hooks

| Hook | When | Can Block |
|------|------|-----------|
| `PreToolUse` | Before tool execution | Yes |
| `PostToolUse` | After tool execution | No |
| `Stop` | Agent finishes responding | Yes |
| `SubagentStop` | Subagent completes | No |
| `SessionStart` | Session initialization | No |
| `SessionEnd` | Session cleanup | No |
| `UserPromptSubmit` | Before processing user input | Yes |
| `Notification` | Claude sends notifications | No |
| `PreCompact` | Before context compaction | No |

## Hook Interface

**Input:** JSON via stdin with event metadata

**Output:** Exit codes
- `0` = Success
- `2` = Blocking error (feedback to Claude)
- Other = Non-blocking error (shown to user)

**Environment Variables:**
- `CLAUDE_PLUGIN_ROOT` - Plugin directory path
- `CLAUDE_PROJECT_DIR` - Project root directory
- `CLAUDE_SESSION_ID` - Session identifier

## Creating a Plugin

1. Create plugin directory:
   ```bash
   mkdir -p .claude-plugin/hooks
   ```

2. Create `marketplace.json`:
   ```json
   {
     "name": "My Plugin",
     "version": "0.1.0",
     "description": "What it does",
     "author": "Your Name",
     "hooks": "./hooks/hooks.json"
   }
   ```

3. Create `hooks/hooks.json`:
   ```json
   {
     "PostToolUse": [{
       "matcher": "*",
       "hooks": [{"type": "command", "command": "${CLAUDE_PLUGIN_ROOT}/hooks/my-hook.sh"}]
     }]
   }
   ```

4. Create hook script `hooks/my-hook.sh` and make executable:
   ```bash
   #!/usr/bin/env bash
   cat | jq '.'  # Process JSON from stdin
   ```

## Resources

- [Claude Code Plugin Documentation](https://code.claude.com/docs/en/plugin-marketplaces)
- [Hooks Reference](https://code.claude.com/docs/en/hooks)
