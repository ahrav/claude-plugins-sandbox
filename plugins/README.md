# Claude Code Plugins

Custom Claude Code plugins built as standalone Rust binaries for observability and tooling integration.

## Current Plugins

### Talon (`talon/`)
Observability system for capturing and forwarding Claude Code hook events to trace collectors.

**Components:**
- `talon-tap` - Lightweight hook forwarder (reads stdin, sends to agent via IPC)
- `talon-agent` - Background daemon (batching, compression, HTTP delivery, disk spooling)

**Use case:** Real-time trace collection with automatic retries and offline spooling.

## Quick Build

Build a specific plugin:
```bash
cd plugins/talon
cargo build --release
```

Build all plugins:
```bash
# From repository root
for plugin in plugins/*/Cargo.toml; do
    (cd $(dirname "$plugin") && cargo build --release)
done
```

Binaries output to `target/release/` within each plugin directory.

## Plugin Organization

Plugins can be structured as:
- **Single package** - Multiple binaries in `src/bin/`, shared dependencies (e.g., talon)
- **Cargo workspace** - Independent crates with separate versioning

Choose based on whether components share dependencies and benefit from unified versioning.

## Installation

Add built binaries to your PATH or reference them directly in Claude Code hooks configuration.

## Documentation

- **Complete setup guide:** [../docs/SETUP.md](../docs/SETUP.md)
- **Talon details:** [talon/README.md](./talon/README.md)
