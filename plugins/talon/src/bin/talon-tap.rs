//! Talon Tap - Lightweight hook event forwarder
//!
//! Reads JSON from stdin, annotates with metadata, and forwards to talon-agent via IPC.
//! Designed to be fast and minimal to avoid blocking Claude Code hooks.

use clap::Parser;
use std::{
    env,
    io::{self, Read, Write},
    process::Command,
    time::Duration,
};

/// CLI arguments for talon-tap
#[derive(Parser)]
#[command(
    author,
    version,
    about = "Lightweight hook event forwarder - forwards events from Claude Code to talon-agent"
)]
struct Cli {
    /// Event type name (e.g., "pre_commit", "post_tool_use")
    #[arg(long, default_value = "unknown")]
    event: String,
}

/// Sends payload to the agent via Unix domain socket.
///
/// Unix sockets are preferred on *nix systems for IPC because they offer better
/// performance and security than TCP (filesystem permissions, no network exposure).
///
/// # Arguments
///
/// * `ipc_path` - Path to the Unix socket (e.g., `/tmp/talon.sock`)
/// * `payload` - JSON event data to send
///
/// # Returns
///
/// * `Ok(())` if the payload was successfully sent and flushed
/// * `Err(io::Error)` if the connection fails or write fails (agent not running, etc.)
///
/// # Errors
///
/// Returns an error if:
/// - The agent is not running (connection refused)
/// - The socket path doesn't exist or has wrong permissions
/// - The write or flush operation fails
#[cfg(unix)]
fn try_send(ipc_path: &str, payload: &[u8]) -> io::Result<()> {
    use std::os::unix::net::UnixStream;
    let mut stream = UnixStream::connect(ipc_path)?;
    stream.write_all(payload)?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    Ok(())
}

/// Sends payload to the agent via TCP (Windows fallback).
///
/// Windows doesn't have reliable Unix domain socket support in all versions,
/// so we fall back to TCP on localhost. The hardcoded port (7878) must match
/// the agent's listening port.
///
/// # Arguments
///
/// * `_ipc_path` - Ignored on Windows (kept for API compatibility)
/// * `payload` - JSON event data to send
///
/// # Returns
///
/// * `Ok(())` if the payload was successfully sent and flushed
/// * `Err(io::Error)` if the connection fails or write fails
///
/// # Errors
///
/// Returns an error if:
/// - The agent is not listening on 127.0.0.1:7878
/// - The write or flush operation fails
#[cfg(not(unix))]
fn try_send(_ipc_path: &str, payload: &[u8]) -> io::Result<()> {
    use std::net::TcpStream;
    let mut stream = TcpStream::connect("127.0.0.1:7878")?;
    stream.write_all(payload)?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    Ok(())
}

/// Auto-starts the agent if it's not running.
///
/// Uses `TALON_AGENT_PATH` environment variable to locate the agent binary,
/// falling back to `PATH` lookup if not set. This allows installations in
/// non-standard locations or testing with local builds.
///
/// Configuration is passed through environment variables rather than a config file
/// to support containerized and ephemeral environments where file-based config
/// would be more complex to manage.
///
/// # Arguments
///
/// * `ipc_path` - Socket path to pass to the agent's `--sock` flag
///
/// # Returns
///
/// * `Ok(())` if the agent process was spawned successfully (does not wait for readiness)
/// * `Err(io::Error)` if spawning fails (binary not found, permission denied, etc.)
///
/// # Errors
///
/// Returns an error if:
/// - The agent binary cannot be found in `TALON_AGENT_PATH` or `PATH`
/// - The binary lacks execute permissions
/// - Process spawning fails for any system reason
fn start_agent(ipc_path: &str) -> io::Result<()> {
    let agent_path = env::var("TALON_AGENT_PATH").unwrap_or_else(|_| "talon-agent".into());
    let mut cmd = Command::new(agent_path);
    cmd.arg("start");

    // Pass through trace backend configuration from environment
    if let Ok(endpoint) = env::var("TRACE_ENDPOINT") {
        cmd.arg("--endpoint").arg(endpoint);
    }
    if let Ok(key) = env::var("TRACE_API_KEY") {
        cmd.arg("--api-key").arg(key);
    }
    if !ipc_path.is_empty() {
        cmd.arg("--sock").arg(ipc_path);
    }

    cmd.spawn()?;
    Ok(())
}

fn main() {
    let cli = Cli::parse();

    // Bounded buffer prevents memory exhaustion if hooks accidentally pipe
    // large files (e.g., `cat large.json | talon-tap`). 2MB is large enough
    // for any realistic hook payload while preventing DoS.
    let max_bytes = env::var("TALON_TAP_MAX_STDIN_BYTES")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(2 * 1024 * 1024);

    let mut reader = io::stdin().lock().take(max_bytes);
    let mut buffer = String::new();
    let _ = reader.read_to_string(&mut buffer);

    // Fallback to empty object on parse failure ensures we always forward *something*
    // rather than silently dropping events. The agent can log malformed payloads.
    let payload_json: serde_json::Value =
        serde_json::from_str(buffer.trim()).unwrap_or_else(|_| serde_json::json!({}));

    // CLAUDE_SESSION_ID enables correlating events across a single IDE session
    // HOSTNAME/whoami fallback supports containerized environments where HOSTNAME may not be set
    let env_metadata = serde_json::json!({
        "session_id": env::var("CLAUDE_SESSION_ID").ok(),
        "host": env::var("HOSTNAME").ok().or_else(|| whoami::fallible::hostname().ok()),
        "pid": std::process::id(),
    });

    let envelope = serde_json::json!({
        "event": cli.event,
        "payload": payload_json,
        "ts": chrono::Utc::now().to_rfc3339(),
        "env": env_metadata,
        "plugin": "talon",
        "version": env!("CARGO_PKG_VERSION"),
    });

    let serialized = serde_json::to_string(&envelope).expect("serialize envelope");
    let ipc_path = env::var("TALON_SOCK").unwrap_or_else(|_| "/tmp/talon.sock".into());

    // Retry logic: If agent isn't running, start it and retry exactly once.
    // This avoids infinite retry loops while handling the common cold-start case.
    // 150ms sleep gives the agent time to create its socket before we reconnect.
    let sent = try_send(&ipc_path, serialized.as_bytes())
        .or_else(|_| {
            start_agent(&ipc_path)?;
            std::thread::sleep(Duration::from_millis(150));
            try_send(&ipc_path, serialized.as_bytes())
        })
        .is_ok();

    if !sent {
        eprintln!("talon-tap: failed to send event to agent");
        std::process::exit(1);
    }
}
