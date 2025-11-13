# Claude Code Plugins

This directory contains custom Claude Code plugins built as standalone Rust binaries.

## Organization

Each plugin is organized as a subdirectory:

```
plugins/
├── README.md
├── talon/          # Observability tap & agent
└── [future]/       # Additional plugins go here
```

## Plugin Structure

Plugins can use either:
- **Single package** (shared dependencies, multiple binaries in `src/bin/`)
- **Cargo workspace** (independent versioning, separate crates)

The choice depends on whether the plugin's components benefit from shared dependencies and unified versioning.

## Building Plugins

To build a specific plugin:

```bash
cd plugins/talon
cargo build --release
```

To build all plugins:

```bash
# From repository root
for plugin in plugins/*/Cargo.toml; do
    (cd $(dirname "$plugin") && cargo build --release)
done
```

## Installing Plugins

Built binaries are located in `target/release/` within each plugin directory. Install them to your PATH or reference them directly in Claude Code hooks configuration.
