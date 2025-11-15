//! Background daemon for batching and forwarding trace events.
//!
//! Accepts events from talon-tap via IPC, batches efficiently, and forwards
//! to a trace collector with retry logic and disk spooling.

mod map;
mod schema;

use crate::map::from_tap_frame;
use crate::schema::canonicalize;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use crossbeam_channel as chan;
use flate2::{Compression, write::GzEncoder};
use fs2::FileExt;
use serde_json::Value as Json;
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

/// Configuration for the agent runtime
struct Config {
    endpoint: String,
    api_key: Option<String>,
    batch_size: usize,
    batch_ms: u64,
    chan_capacity: usize,
    batch_bytes: usize,
    spool_dir: PathBuf,
    spool_bytes: u64,
}

/// RAII guard for spool directory lock.
///
/// Automatically releases the lock on drop, preventing lock leaks
/// and ensuring proper cleanup on panic or early return.
struct SpoolLockGuard {
    _file: File,
}

impl SpoolLockGuard {
    /// Acquire exclusive lock on the spool directory.
    ///
    /// Creates the lock file if it doesn't exist and blocks until
    /// the lock is acquired.
    fn acquire(dir: &Path) -> Result<Self> {
        let lock_path = dir.join(".spool.lock");
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .open(&lock_path)
            .context("failed to open spool lock file")?;

        file.lock_exclusive()
            .context("failed to acquire spool directory lock")?;

        Ok(Self { _file: file })
    }
}

impl Drop for SpoolLockGuard {
    fn drop(&mut self) {
        // Unlock is automatic when the file descriptor closes,
        // but we can explicitly unlock for clarity
        let _ = self._file.unlock();
    }
}

#[derive(Parser)]
#[command(author, version, about = "Talon observability agent")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start the agent daemon
    Start {
        #[arg(long, default_value = "/tmp/talon.sock")]
        sock: String,

        #[arg(long, env = "TRACE_ENDPOINT")]
        endpoint: String,

        #[arg(long, env = "TRACE_API_KEY")]
        api_key: Option<String>,

        #[arg(long, default_value_t = 100)]
        batch_size: usize,

        #[arg(long, default_value_t = 200)]
        batch_ms: u64,

        #[arg(long, default_value_t = 10_000)]
        chan_capacity: usize,

        #[arg(long, default_value_t = 1_048_576)]
        batch_bytes: usize,

        #[arg(long, default_value_t = 50_000_000)]
        spool_bytes: u64,

        #[arg(long)]
        spool_dir: Option<PathBuf>,
    },

    /// Manually flush spooled events
    Flush {
        #[arg(long, env = "TRACE_ENDPOINT")]
        endpoint: String,

        #[arg(long, env = "TRACE_API_KEY")]
        api_key: Option<String>,

        #[arg(long)]
        spool_dir: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.cmd {
        Cmd::Start {
            sock,
            endpoint,
            api_key,
            batch_size,
            batch_ms,
            chan_capacity,
            batch_bytes,
            spool_bytes,
            spool_dir,
        } => {
            let spool_dir = spool_dir.unwrap_or(default_spool_dir()?);
            fs::create_dir_all(&spool_dir).ok();

            let config = Config {
                endpoint,
                api_key,
                batch_size,
                batch_ms,
                chan_capacity,
                batch_bytes,
                spool_dir,
                spool_bytes,
            };

            #[cfg(unix)]
            return run_unix(sock, config);

            #[cfg(not(unix))]
            return run_tcp("127.0.0.1:7878".to_string(), config);
        }

        Cmd::Flush {
            endpoint,
            api_key,
            spool_dir,
        } => {
            let spool_dir = spool_dir.unwrap_or(default_spool_dir()?);
            let client = http_client()?;
            flush_spool(&client, &endpoint, api_key.as_deref(), &spool_dir)?;
            Ok(())
        }
    }
}

/// Run agent with Unix socket listener.
///
/// Uses Unix domain sockets for better security (filesystem permissions) and lower
/// overhead than TCP. Socket secured with 0o600 permissions.
#[cfg(unix)]
fn run_unix(sock: String, config: Config) -> Result<()> {
    use std::os::unix::net::UnixListener;

    // Clean up stale socket
    let _ = fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).with_context(|| format!("bind UDS {}", sock))?;

    // Secure socket: owner read-write only
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&sock, fs::Permissions::from_mode(0o600)).ok();
    }

    let (tx, rx) = chan::bounded::<String>(config.chan_capacity);
    let client = http_client()?;

    // Spawn HTTP sender thread
    thread::spawn(move || http_loop(rx, client, config));

    // Accept connections
    for stream in listener.incoming().flatten() {
        let txc = tx.clone();
        thread::spawn(move || handle_conn_unix(stream, txc));
    }

    Ok(())
}

/// Run agent with TCP listener (Windows fallback).
///
/// Binds to localhost (127.0.0.1) to reduce security risks.
#[cfg(not(unix))]
fn run_tcp(addr: String, config: Config) -> Result<()> {
    use std::net::TcpListener;

    let listener = TcpListener::bind(&addr).with_context(|| format!("bind TCP {}", addr))?;
    let (tx, rx) = chan::bounded::<String>(config.chan_capacity);
    let client = http_client()?;

    thread::spawn(move || http_loop(rx, client, config));

    for stream in listener.incoming() {
        if let Ok(stream) = stream {
            let txc = tx.clone();
            thread::spawn(move || handle_conn_tcp(stream, txc));
        }
    }

    Ok(())
}

/// Handle Unix socket connection.
///
/// Reads line-delimited JSON frames and forwards to the batching channel.
/// Blocks on channel send to apply backpressure.
#[cfg(unix)]
fn handle_conn_unix(stream: std::os::unix::net::UnixStream, tx: chan::Sender<String>) {
    let reader = BufReader::new(stream);
    for line in reader.lines().map_while(Result::ok) {
        if !line.trim().is_empty() {
            // Block on send to apply backpressure
            let _ = tx.send(line);
        }
    }
}

/// Handle TCP connection.
///
/// Same behavior as Unix socket handler but over TCP.
#[cfg(not(unix))]
fn handle_conn_tcp(stream: std::net::TcpStream, tx: chan::Sender<String>) {
    let reader = BufReader::new(stream);
    for line in reader.lines().flatten() {
        if !line.trim().is_empty() {
            let _ = tx.send(line);
        }
    }
}

/// Create HTTP client with 8s timeout and connection pooling.
fn http_client() -> Result<reqwest::blocking::Client> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(8))
        .pool_idle_timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(8)
        .build()?;
    Ok(client)
}

/// Main batching and sending loop.
///
/// Accumulates events and flushes when any trigger fires:
/// - **Count trigger**: `batch_size` events accumulated
/// - **Byte trigger**: `batch_bytes` accumulated
/// - **Time trigger**: `batch_ms` elapsed
///
/// Failed sends spool to disk for retry. Malformed events quarantine for debugging.
/// After successful sends, attempts to drain spooled events.
fn http_loop(rx: chan::Receiver<String>, client: reqwest::blocking::Client, config: Config) {
    let mut buf: Vec<Json> = Vec::with_capacity(config.batch_size);
    let mut buf_bytes: usize = 0;
    let mut last = Instant::now();

    // Try to drain any existing spooled events from previous runs
    let _ = flush_spool(
        &client,
        &config.endpoint,
        config.api_key.as_deref(),
        &config.spool_dir,
    );

    let timeout = Duration::from_millis(config.batch_ms);

    loop {
        match rx.recv_timeout(timeout) {
            Ok(line) => {
                // Parse tap frame -> map to canonical TraceV1 -> push to batch buffer
                match serde_json::from_str::<Json>(&line) {
                    Ok(frame) => match from_tap_frame(frame) {
                        Ok(mut rec) => {
                            canonicalize(&mut rec);
                            let json_rec = serde_json::to_value(&rec)
                                .unwrap_or_else(|_| Json::Object(Default::default()));
                            let sz = json_rec.to_string().len();
                            buf.push(json_rec);
                            buf_bytes += sz;
                        }
                        Err(e) => {
                            let _ = append_to_quarantine(&config.spool_dir, &line, e.to_string());
                        }
                    },
                    Err(e) => {
                        let _ = append_to_quarantine(
                            &config.spool_dir,
                            &line,
                            format!("parse error: {e}"),
                        );
                    }
                }
            }
            Err(chan::RecvTimeoutError::Timeout) => {}
            Err(chan::RecvTimeoutError::Disconnected) => break,
        }

        // Check if any of the three flush triggers have fired
        let time_due = last.elapsed() >= timeout && !buf.is_empty();
        let size_due = buf.len() >= config.batch_size || buf_bytes >= config.batch_bytes;

        if time_due || size_due {
            if send_batch(&client, &config.endpoint, config.api_key.as_deref(), &buf).is_err() {
                // On failure, spool to disk for later retry
                let _ = append_to_spool(&config.spool_dir, &buf, config.spool_bytes);
            }
            buf.clear();
            buf_bytes = 0;
            last = Instant::now();

            // Opportunistically drain spool after successful send
            let _ = flush_spool(
                &client,
                &config.endpoint,
                config.api_key.as_deref(),
                &config.spool_dir,
            );
        }
    }
}

/// Send a batch of events to the collector with retry logic.
///
/// Serializes to JSON, compresses with gzip, and POSTs to collector.
///
/// Retries up to 4 times with exponential backoff (200ms base, doubles each attempt)
/// and ±50% jitter. Retries 5xx and network errors, but not 4xx client errors.
///
/// # Errors
///
/// Returns error if all retries exhausted or 4xx response received.
fn send_batch(
    client: &reqwest::blocking::Client,
    endpoint: &str,
    api_key: Option<&str>,
    events: &[Json],
) -> Result<()> {
    if events.is_empty() {
        return Ok(());
    }

    // Serialize and compress (typically 5-10x size reduction)
    let body_json = serde_json::to_vec(events)?;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&body_json)?;
    let body_gz = encoder.finish()?;

    let mut req = client
        .post(endpoint)
        .header("Content-Type", "application/json")
        .header("Content-Encoding", "gzip");

    if let Some(key) = api_key {
        req = req.bearer_auth(key);
    }

    // Retry with exponential backoff + jitter
    let mut delay = Duration::from_millis(200);
    for attempt in 0..4 {
        match req.try_clone().unwrap().body(body_gz.clone()).send() {
            Ok(resp) if resp.status().is_success() => return Ok(()),
            Ok(resp) if resp.status().is_client_error() => {
                // Don't retry 4xx - client errors won't resolve on retry
                return Err(anyhow::anyhow!("collector returned 4xx: {}", resp.status()));
            }
            Ok(_) | Err(_) => {
                // Retry 5xx server errors and network failures
                if attempt < 3 {
                    thread::sleep(jitter(delay));
                    delay = delay.saturating_mul(2);
                }
            }
        }
    }

    Err(anyhow::anyhow!("send failed after retries"))
}

/// Add random jitter to duration (±50%).
///
/// Prevents thundering herd when multiple agents retry simultaneously.
fn jitter(d: Duration) -> Duration {
    use rand::Rng;
    let ms = d.as_millis() as u64;
    let jittered = rand::rng().random_range((ms / 2)..=(ms + ms / 2));
    Duration::from_millis(jittered)
}

/// Get default spool directory.
fn default_spool_dir() -> Result<PathBuf> {
    let base = dirs_next::data_local_dir().unwrap_or_else(std::env::temp_dir);
    Ok(base.join("talon").join("spool"))
}

/// Append events to spool file, rotating if it exceeds cap.
///
/// Writes events as line-delimited JSON for later retry via `flush_spool`.
///
/// When file exceeds `cap_bytes`, keeps last 50% of lines (drops oldest) to bound
/// disk usage while preserving recent events.
///
/// Uses directory-level locking to prevent race conditions during rotation.
/// Calls `sync_all()` to ensure durability on crash.
fn append_to_spool(dir: &Path, events: &[Json], cap_bytes: u64) -> Result<()> {
    fs::create_dir_all(dir)
        .with_context(|| format!("failed to create spool directory: {}", dir.display()))?;
    let file_path = dir.join("events.jsonl");

    // Acquire directory-level lock via RAII guard
    let _lock = SpoolLockGuard::acquire(dir)?;

    // Open file and write events while holding directory lock
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file_path)?;

    for event in events {
        let line = serde_json::to_string(event)?;
        f.write_all(line.as_bytes())?;
        f.write_all(b"\n")?;
    }

    // Ensure data is written to disk for crash safety
    f.sync_all()
        .context("failed to sync spool file to disk")?;
    drop(f);

    // Check rotation while still holding directory lock
    let needs_rotation = file_path
        .metadata()
        .map(|m| m.len())
        .unwrap_or(0)
        > cap_bytes;

    // Rotate if needed while holding directory lock
    // This prevents another thread from creating a new file during rotation
    if needs_rotation {
        rotate_spool_file(dir, &file_path)?;
    }

    // Lock automatically released when _lock goes out of scope
    Ok(())
}

/// Rotate spool file by keeping last 50% of lines.
///
/// MUST be called while holding the directory lock (from append_to_spool).
/// Does NOT acquire its own lock - relies on caller's directory lock.
/// Syncs both file data and directory metadata for crash safety.
fn rotate_spool_file(dir: &Path, file_path: &Path) -> Result<()> {
    let tmp = dir.join("events.tmp");

    // Perform rotation (caller holds directory lock)
    fs::rename(file_path, &tmp)
        .context("failed to rename spool file for rotation")?;

    // TESTING: Add artificial delay to widen race window for test verification
    #[cfg(test)]
    thread::sleep(Duration::from_millis(5));

    // Read and keep last 50% of lines
    let reader = BufReader::new(File::open(&tmp)?);
    let lines: Vec<String> = reader.lines().map_while(Result::ok).collect();
    let keep_from = lines.len().saturating_sub(lines.len() / 2);
    let keep = &lines[keep_from..];

    // Write kept lines to new file
    let mut out = File::create(file_path)?;
    for line in keep {
        writeln!(out, "{}", line)?;
    }

    // Ensure file data is durable before cleaning up temp file
    out.sync_all()
        .context("failed to sync rotated spool file to disk")?;
    drop(out);

    // Sync directory to persist the rename operation
    #[cfg(unix)]
    {
        let dir_fd = File::open(dir)?;
        dir_fd
            .sync_all()
            .context("failed to sync directory metadata")?;
    }

    // Cleanup temp file
    fs::remove_file(&tmp)
        .with_context(|| format!("failed to remove temp file: {}", tmp.display()))?;

    Ok(())
}

/// Flush spooled events to the collector.
///
/// Called on startup, after successful sends, and via `talon-agent flush` command.
///
/// Sends in batches of 500. Clears spool only after all events successfully send.
///
/// Uses directory-level locking to prevent concurrent modification during flush.
/// Syncs after clearing to ensure durability.
///
/// # Errors
///
/// Returns error on first send failure.
fn flush_spool(
    client: &reqwest::blocking::Client,
    endpoint: &str,
    api_key: Option<&str>,
    dir: &Path,
) -> Result<()> {
    let file_path = dir.join("events.jsonl");
    if !file_path.exists() {
        return Ok(());
    }

    // Acquire directory-level lock via RAII guard
    let _lock = SpoolLockGuard::acquire(dir)?;

    // Read and send events while holding lock
    let reader = BufReader::new(File::open(&file_path)?);
    let mut batch: Vec<Json> = Vec::new();

    for line in reader.lines().map_while(Result::ok) {
        if let Ok(val) = serde_json::from_str::<Json>(&line) {
            batch.push(val);
            if batch.len() >= 500 {
                send_batch(client, endpoint, api_key, &batch)?;
                batch.clear();
            }
        }
    }

    if !batch.is_empty() {
        send_batch(client, endpoint, api_key, &batch)?;
    }

    // Clear spool file only after all events successfully sent
    let cleared = File::create(&file_path)
        .context("failed to clear spool file after successful flush")?;

    // Ensure the truncation is durable
    cleared
        .sync_all()
        .context("failed to sync cleared spool file")?;

    // Lock automatically released when _lock goes out of scope
    Ok(())
}

/// Append malformed events to quarantine file for debugging.
///
/// Isolates parse/mapping failures to `quarantine.jsonl` for inspection without
/// blocking the pipeline.
fn append_to_quarantine(dir: &Path, raw_line: &str, reason: String) -> Result<()> {
    fs::create_dir_all(dir).ok();
    let file = dir.join("quarantine.jsonl");
    let mut f = OpenOptions::new().create(true).append(true).open(&file)?;
    let rec = serde_json::json!({ "reason": reason, "raw": raw_line });
    writeln!(f, "{}", rec)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;
    use tempfile::TempDir;

    /// Helper to create test events
    fn test_event(id: usize) -> Json {
        serde_json::json!({
            "event": "test",
            "id": id,
            "timestamp": "2025-11-13T00:00:00Z"
        })
    }

    /// Helper to read events from spool file
    fn read_spool_events(dir: &Path) -> Vec<String> {
        let file = dir.join("events.jsonl");
        if !file.exists() {
            return Vec::new();
        }
        std::fs::read_to_string(file)
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    #[test]
    fn test_append_to_spool_basic() {
        let temp_dir = TempDir::new().unwrap();
        let events = vec![test_event(1), test_event(2), test_event(3)];

        let result = append_to_spool(temp_dir.path(), &events, 1_000_000);
        assert!(result.is_ok());

        let lines = read_spool_events(temp_dir.path());
        assert_eq!(lines.len(), 3);

        // Verify each line is valid JSON
        for line in &lines {
            let parsed: Result<Json, _> = serde_json::from_str(line);
            assert!(parsed.is_ok(), "Failed to parse: {}", line);
        }
    }

    #[test]
    fn test_append_to_spool_rotation_keeps_last_50_percent() {
        let temp_dir = TempDir::new().unwrap();

        // Create 20 small events
        let events: Vec<Json> = (0..20).map(test_event).collect();

        // Set cap to trigger rotation after ~10 events (each event is ~60-70 bytes)
        let cap_bytes = 700;

        // First append: 10 events (~600 bytes, below cap)
        append_to_spool(temp_dir.path(), &events[0..10], cap_bytes).unwrap();
        let lines_after_first = read_spool_events(temp_dir.path());
        println!("After first append: {} events", lines_after_first.len());

        // Second append: 10 more events (total ~1200 bytes, exceeds cap)
        append_to_spool(temp_dir.path(), &events[10..20], cap_bytes).unwrap();

        // After rotation, should keep approximately last 50% of lines
        let final_lines = read_spool_events(temp_dir.path());
        println!("After rotation: {} events", final_lines.len());

        // With 20 events total and rotation at 700 bytes, we should have
        // kept the last 50% (approximately 10 events)
        assert!(
            final_lines.len() >= 8 && final_lines.len() <= 12,
            "Expected ~10 events (50%) after rotation, got {}",
            final_lines.len()
        );

        // Verify the kept events are the most recent ones (higher IDs)
        let kept_ids: Vec<i64> = final_lines
            .iter()
            .filter_map(|line| serde_json::from_str::<Json>(line).ok())
            .filter_map(|event| event.get("id").and_then(|v| v.as_i64()))
            .collect();

        // Should have events from the second half (IDs >= 10)
        assert!(
            kept_ids.iter().any(|&id| id >= 10),
            "Should have kept some events from second append"
        );
    }

    #[test]
    fn test_append_to_spool_concurrent_writes_no_corruption() {
        let temp_dir = TempDir::new().unwrap();
        let dir = Arc::new(temp_dir.path().to_path_buf());
        let cap_bytes = 1_000_000; // Large cap to avoid rotation

        // Spawn 10 threads, each writing 10 events concurrently
        let handles: Vec<_> = (0..10)
            .map(|thread_id| {
                let dir = Arc::clone(&dir);
                thread::spawn(move || {
                    let events: Vec<Json> = (0..10)
                        .map(|i| {
                            serde_json::json!({
                                "thread": thread_id,
                                "event_id": i,
                                "timestamp": "2025-11-13T00:00:00Z"
                            })
                        })
                        .collect();

                    append_to_spool(&dir, &events, cap_bytes).expect("append failed");
                })
            })
            .collect();

        // Wait for all threads
        for handle in handles {
            handle.join().expect("thread panicked");
        }

        // Verify all 100 events were written (10 threads × 10 events)
        let lines = read_spool_events(&dir);
        assert_eq!(lines.len(), 100, "Expected 100 events, got {}", lines.len());

        // Verify each line is valid JSON (no corruption)
        for (i, line) in lines.iter().enumerate() {
            let parsed: Result<Json, _> = serde_json::from_str(line);
            assert!(
                parsed.is_ok(),
                "Line {} corrupted or invalid JSON: {}",
                i,
                line
            );
        }

        // Verify we have events from all 10 threads
        let thread_ids: std::collections::HashSet<i64> = lines
            .iter()
            .filter_map(|line| serde_json::from_str::<Json>(line).ok())
            .filter_map(|event| event.get("thread").and_then(|v| v.as_i64()))
            .collect();

        assert_eq!(
            thread_ids.len(),
            10,
            "Expected events from 10 threads, got {}",
            thread_ids.len()
        );
    }

    #[test]
    fn test_flush_spool_clears_file_on_success() {
        let temp_dir = TempDir::new().unwrap();

        // Write some events to spool
        let events = vec![test_event(1), test_event(2), test_event(3)];
        append_to_spool(temp_dir.path(), &events, 1_000_000).unwrap();

        // Verify events exist
        assert_eq!(read_spool_events(temp_dir.path()).len(), 3);

        // Create mock HTTP server that always returns 200
        let mut mock_server = mockito::Server::new();
        let mock = mock_server
            .mock("POST", "/")
            .with_status(200)
            .with_body(r#"{"status":"ok"}"#)
            .create();

        // Flush spool
        let client = http_client().unwrap();
        let result = flush_spool(&client, &mock_server.url(), None, temp_dir.path());

        assert!(result.is_ok(), "flush_spool failed: {:?}", result.err());

        // Verify spool file is empty after successful flush
        let lines_after = read_spool_events(temp_dir.path());
        assert_eq!(
            lines_after.len(),
            0,
            "Spool should be empty after successful flush"
        );

        // Verify HTTP request was made
        mock.assert();
    }

    #[test]
    fn test_flush_spool_batches_of_500() {
        let temp_dir = TempDir::new().unwrap();

        // Write 1200 events to trigger multiple batches
        let events: Vec<Json> = (0..1200).map(test_event).collect();
        append_to_spool(temp_dir.path(), &events, 10_000_000).unwrap();

        // Create mock server that counts requests
        let mut mock_server = mockito::Server::new();
        let mock = mock_server
            .mock("POST", "/")
            .with_status(200)
            .expect(3) // Should be 3 batches: 500 + 500 + 200
            .create();

        // Flush spool
        let client = http_client().unwrap();
        let result = flush_spool(&client, &mock_server.url(), None, temp_dir.path());

        assert!(result.is_ok());

        // Verify all 3 requests were made
        mock.assert();
    }

    #[test]
    fn test_jitter_range() {
        let base = Duration::from_millis(200);

        // Test jitter 100 times to verify range
        for _ in 0..100 {
            let jittered = jitter(base);
            let ms = jittered.as_millis();

            // Jitter should be ±50%: 100ms to 300ms
            assert!(
                ms >= 100 && ms <= 300,
                "Jittered delay {}ms out of expected range [100, 300]",
                ms
            );
        }
    }

    #[test]
    fn test_append_to_quarantine() {
        let temp_dir = TempDir::new().unwrap();

        let result = append_to_quarantine(
            temp_dir.path(),
            r#"{invalid json}"#,
            "parse error".to_string(),
        );

        assert!(result.is_ok());

        // Read quarantine file
        let quarantine_file = temp_dir.path().join("quarantine.jsonl");
        let content = std::fs::read_to_string(quarantine_file).unwrap();

        // Verify format
        let entry: Json = serde_json::from_str(&content.trim()).unwrap();
        assert_eq!(entry["reason"], "parse error");
        assert_eq!(entry["raw"], "{invalid json}");
    }

    #[test]
    fn test_default_spool_dir_returns_valid_path() {
        let result = default_spool_dir();
        assert!(result.is_ok());

        let path = result.unwrap();
        assert!(path.to_string_lossy().contains("talon"));
        assert!(path.to_string_lossy().contains("spool"));
    }

    /// Test that concurrent appends during rotation do NOT lose data.
    ///
    /// Verifies the fix for the race condition where:
    /// 1. Thread A renames events.jsonl -> events.tmp (lock released on renamed file)
    /// 2. Thread B creates NEW events.jsonl and appends data
    /// 3. Thread A does File::create() which truncates Thread B's data
    #[test]
    fn test_rotation_does_not_lose_concurrent_appends() {
        use std::sync::{Arc, Barrier};

        // Run test multiple times since race conditions can be timing-dependent
        for _ in 0..20 {
            let temp_dir = TempDir::new().unwrap();
            let dir = Arc::new(temp_dir.path().to_path_buf());
            let file_path = dir.join("events.jsonl");

            // Create initial file
            let initial_events: Vec<Json> = (0..1000).map(test_event).collect();
            append_to_spool(&dir, &initial_events, 1_000_000).unwrap();

            // Synchronize threads to maximize chance of hitting race window
            let barrier = Arc::new(Barrier::new(2));
            let barrier_clone = Arc::clone(&barrier);
            let dir_clone = Arc::clone(&dir);
            let file_path_clone = file_path.clone();

            // Thread 1: Perform rotation
            let rotation_handle = thread::spawn(move || {
                barrier_clone.wait();

                // Acquire directory lock via RAII guard
                let _lock = SpoolLockGuard::acquire(&dir_clone).unwrap();

                // Call rotate_spool_file while holding lock
                rotate_spool_file(&dir_clone, &file_path_clone).unwrap();

                // Lock automatically released when _lock goes out of scope
            });

            // Thread 2: Append during rotation window
            let dir_clone2 = Arc::clone(&dir);
            let append_handle = thread::spawn(move || {
                barrier.wait();
                thread::sleep(Duration::from_millis(2));

                // Append critical events that should NOT be lost
                let critical_events: Vec<Json> = (9000..9010)
                    .map(|i| {
                        serde_json::json!({
                            "event": "CRITICAL",
                            "id": i,
                            "timestamp": "2025-11-14T00:00:00Z"
                        })
                    })
                    .collect();

                append_to_spool(&dir_clone2, &critical_events, 1_000_000).unwrap();
            });

            rotation_handle.join().unwrap();
            append_handle.join().unwrap();

            // Verify all critical events were preserved
            let final_lines = read_spool_events(&dir);
            let critical_count = final_lines
                .iter()
                .filter_map(|line| serde_json::from_str::<Json>(line).ok())
                .filter(|e| e.get("event").and_then(|v| v.as_str()) == Some("CRITICAL"))
                .count();

            assert_eq!(
                critical_count, 10,
                "Data loss during rotation: expected 10 critical events, found {}",
                critical_count
            );
        }
    }

    /// Test that file locking works correctly across separate processes.
    ///
    /// Verifies that the spool directory lock prevents data corruption when
    /// accessed from multiple processes (not just threads). This test:
    /// 1. Creates a temp directory with test events
    /// 2. Spawns a child process running `talon-agent flush`
    /// 3. Concurrently appends events from the parent process
    /// 4. Verifies no data corruption or lock failures occur
    #[test]
    #[cfg(unix)]
    fn test_cross_process_locking() {
        use std::env;
        use std::process::Command;

        let temp_dir = TempDir::new().unwrap();
        let spool_path = temp_dir.path();

        // Write initial events to spool
        let initial_events: Vec<Json> = (0..50).map(test_event).collect();
        append_to_spool(spool_path, &initial_events, 1_000_000).unwrap();

        // Verify initial state
        let lines_before = read_spool_events(spool_path);
        assert_eq!(lines_before.len(), 50, "Initial events not written correctly");

        // Create mock HTTP server for flush to succeed
        let mut mock_server = mockito::Server::new();
        let mock = mock_server
            .mock("POST", "/")
            .with_status(200)
            .with_body(r#"{"status":"ok"}"#)
            .expect_at_least(1)
            .create();

        // Get the path to the test binary
        let test_exe = env::current_exe().unwrap();
        let exe_dir = test_exe.parent().unwrap();

        // Find talon-agent binary - it should be in the same directory as the test
        // or in ../../ (deps -> debug/release)
        let agent_path = if exe_dir.join("talon-agent").exists() {
            exe_dir.join("talon-agent")
        } else {
            exe_dir.parent().unwrap().join("talon-agent")
        };

        // Spawn child process running `talon-agent flush` while parent holds operations
        let spool_dir_str = spool_path.to_str().unwrap();
        let endpoint = mock_server.url();

        // Start parent append in background thread that will try to acquire lock
        let spool_path_clone = spool_path.to_path_buf();
        let append_handle = thread::spawn(move || {
            // Small delay to let child process start first
            thread::sleep(Duration::from_millis(10));

            // Try to append new events while child is flushing
            // This should block until child releases lock, then succeed
            let parent_events: Vec<Json> = (1000..1020)
                .map(|i| {
                    serde_json::json!({
                        "event": "parent_process",
                        "id": i,
                        "timestamp": "2025-11-14T00:00:00Z"
                    })
                })
                .collect();

            append_to_spool(&spool_path_clone, &parent_events, 1_000_000)
        });

        // Start child process - it will try to acquire lock
        let mut child = Command::new(&agent_path)
            .arg("flush")
            .arg("--endpoint")
            .arg(&endpoint)
            .arg("--spool-dir")
            .arg(spool_dir_str)
            .spawn()
            .expect("Failed to spawn talon-agent flush process");

        // Wait for child process to complete
        let status = child.wait().expect("Failed to wait for child process");

        // Wait for parent append to complete
        let append_result = append_handle.join().expect("Parent thread panicked");

        // Verify both processes completed successfully
        assert!(status.success(), "Child process failed with status: {}", status);
        assert!(append_result.is_ok(), "Parent append failed: {:?}", append_result.err());

        // Verify final state: spool should contain parent's events
        // (child may have flushed the initial events, or they may all still be there)
        let final_lines = read_spool_events(spool_path);

        // Count parent events - these should ALWAYS be present
        let parent_count = final_lines
            .iter()
            .filter_map(|line| serde_json::from_str::<Json>(line).ok())
            .filter(|e| e.get("event").and_then(|v| v.as_str()) == Some("parent_process"))
            .count();

        // Parent events should be present regardless of race outcome
        assert_eq!(
            parent_count, 20,
            "Expected 20 parent events, found {}. Final lines: {}",
            parent_count,
            final_lines.len()
        );

        // Verify no corruption: all lines should be valid JSON
        for (i, line) in final_lines.iter().enumerate() {
            let parsed: Result<Json, _> = serde_json::from_str(line);
            assert!(
                parsed.is_ok(),
                "Line {} corrupted or invalid JSON: {}",
                i,
                line
            );
        }

        // Count initial events that might remain if parent appended before child flushed
        let initial_count = final_lines
            .iter()
            .filter_map(|line| serde_json::from_str::<Json>(line).ok())
            .filter(|e| {
                e.get("event").and_then(|v| v.as_str()) == Some("test")
                    && e.get("id").and_then(|v| v.as_i64()).map(|id| id < 50).unwrap_or(false)
            })
            .count();

        // Total events should be either:
        // - 20 (parent only, child flushed first) OR
        // - 70 (parent + initial, parent appended before child flushed)
        let total = final_lines.len();
        assert!(
            total == 20 || total == 70,
            "Expected either 20 or 70 total events, found {}. Parent: {}, Initial: {}",
            total,
            parent_count,
            initial_count
        );

        // Verify HTTP mock was called (flush succeeded)
        mock.assert();
    }
}
