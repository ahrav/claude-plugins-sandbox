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
    let base = dirs::data_local_dir().unwrap_or_else(std::env::temp_dir);
    Ok(base.join("talon").join("spool"))
}

/// Append events to spool file, rotating if it exceeds cap.
///
/// Writes events as line-delimited JSON for later retry via `flush_spool`.
///
/// When file exceeds `cap_bytes`, keeps last 50% of lines (drops oldest) to bound
/// disk usage while preserving recent events.
fn append_to_spool(dir: &Path, events: &[Json], cap_bytes: u64) -> Result<()> {
    fs::create_dir_all(dir).ok();
    let file = dir.join("events.jsonl");

    let mut f = OpenOptions::new().create(true).append(true).open(&file)?;
    for event in events {
        let line = serde_json::to_string(event)?;
        f.write_all(line.as_bytes())?;
        f.write_all(b"\n")?;
    }

    // Rotate if file exceeds cap - keep last 50% of lines
    if file.metadata().map(|m| m.len()).unwrap_or(0) > cap_bytes {
        let tmp = dir.join("events.tmp");
        fs::rename(&file, &tmp).ok();

        let reader = BufReader::new(File::open(&tmp)?);
        let lines: Vec<String> = reader.lines().map_while(Result::ok).collect();
        let keep_from = lines.len().saturating_sub(lines.len() / 2);
        let keep = &lines[keep_from..];

        let mut out = File::create(&file)?;
        for line in keep {
            writeln!(out, "{}", line)?;
        }
        let _ = fs::remove_file(tmp);
    }

    Ok(())
}

/// Flush spooled events to the collector.
///
/// Called on startup, after successful sends, and via `talon-agent flush` command.
///
/// Sends in batches of 500. Clears spool only after all events successfully send.
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
    let file = dir.join("events.jsonl");
    if !file.exists() {
        return Ok(());
    }

    let reader = BufReader::new(File::open(&file)?);
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
    File::create(&file)?;
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
