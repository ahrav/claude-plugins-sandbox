# Claude Code Plugins Sandbox

A sandbox environment for building and testing Claude Code plugins with observability and tracing capabilities. This repository demonstrates how to capture tool usage, execution metrics, and runtime traces from Claude Code and send them to observability platforms like Honeycomb, Datadog, or any OpenTelemetry-compatible backend.

## Quick Start

1. **Build the Talon binaries:**
   ```bash
   cd plugins/talon
   cargo build --release
   export PATH="$(pwd)/target/release:$PATH"
   ```

2. **Configure your trace collector:**
   ```bash
   export TRACE_ENDPOINT="https://api.honeycomb.io/v1/traces"
   export TRACE_API_KEY="your-api-key"
   ```

3. **Install the example plugin to your project:**
   ```bash
   cp -r examples/talon-observability/.claude-plugin /path/to/your/project/
   ```

4. **Start using Claude Code in your project** - traces will automatically flow to your observability platform.

## Project Structure

```
claude-plugins-sandbox/
├── plugins/                    # Plugin implementations
│   ├── talon/                 # Observability tap & agent binaries (Rust)
│   │   ├── src/bin/talon-tap.rs      # Hook script that captures events
│   │   └── src/bin/talon-agent/      # Background daemon for batching/forwarding
│   └── README.md              # Plugin development guide
│
├── examples/                   # Integration examples
│   ├── talon-observability/   # Complete observability plugin example
│   └── README.md              # Usage examples and tutorials
│
└── docs/                       # Documentation
    └── SETUP.md               # Complete setup guide
```

## What's Included

### Talon Binaries

Two Rust binaries that work together to provide observability:

- **talon-tap**: Lightweight hook script that captures Claude Code events (tool calls, completions, errors) and forwards them via Unix socket
- **talon-agent**: Background daemon that receives events, batches them, and sends traces to your observability platform

### Example Plugin

A complete working example showing:
- Claude Code plugin structure (`.claude-plugin/` directory)
- Hook configuration (`hooks.json`)
- Event capture patterns using PreToolUse, PostToolUse, and Stop hooks
- Integration with external binaries for processing

## Use Cases

- Monitor Claude Code tool usage and performance in production
- Debug plugin behavior and execution flow
- Track API costs and token usage
- Analyze agent decision patterns
- Build custom observability dashboards
- Alert on errors or unusual patterns

## Prerequisites

- **Rust** 1.70+ (for building Talon binaries)
- **Claude Code** with plugin support enabled
- **Observability platform** (Honeycomb, Datadog, Jaeger, etc.) or any OpenTelemetry-compatible collector

## Documentation

- **[plugins/README.md](plugins/README.md)** - How to build and install plugins
- **[examples/README.md](examples/README.md)** - Integration examples and plugin development guide
- **[Official Docs](https://code.claude.com/docs/en/plugin-marketplaces)** - Claude Code plugin marketplaces
- **[Hooks Reference](https://code.claude.com/docs/en/hooks)** - Available hooks and events

## Architecture

The Talon system uses a tap-and-agent pattern:

1. **Tap** (hook script) runs in the Claude Code process, capturing events with minimal overhead
2. **Agent** (background daemon) runs independently, handling batching, retries, and network I/O
3. **Unix socket** provides fast, reliable IPC between tap and agent
4. **Observability platform** receives structured traces via OpenTelemetry protocol

This separation ensures Claude Code performance isn't impacted by network latency or external service availability.

## Contributing

Contributions welcome! To add new plugins or examples:

1. Create a new directory under `plugins/` or `examples/`
2. Follow the structure shown in existing plugins
3. Include comprehensive README with setup instructions
4. Submit a pull request

## License

MIT OR Apache-2.0
