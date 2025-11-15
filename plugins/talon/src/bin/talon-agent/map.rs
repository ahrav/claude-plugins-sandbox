//! Maps tap frames from instrumented plugins into canonical TraceV1 telemetry events.
//!
//! Supports two input formats:
//! 1. **Fast path**: Pre-formed TraceV1 events (detected via schema_version field)
//! 2. **Fallback path**: Legacy tap wrapper format requiring field extraction
//!
//! The fallback path applies defaults for missing fields and preserves the original
//! payload in extensions for audit purposes.

use crate::schema::*;
use anyhow::{Result, anyhow};
use serde_json::Value as Json;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// Safely converts a JSON value to u32, saturating at u32::MAX if the value exceeds the limit.
///
/// This prevents silent truncation when token counts or latencies exceed 4,294,967,295.
/// Returns 0 if the value is not a valid u64.
fn as_u32_sat(v: &Json) -> u32 {
    v.as_u64().unwrap_or(0).min(u32::MAX as u64) as u32
}

/// Expands tilde (~) in paths to the user's home directory.
///
/// # Examples
/// - `~/foo/bar.txt` → `/Users/username/foo/bar.txt`
/// - `/abs/path.txt` → `/abs/path.txt` (unchanged)
///
/// Returns the original path if home directory cannot be determined.
fn expand_path(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs_next::home_dir() {
            return home.join(rest).to_string_lossy().into_owned();
        }
    } else if path == "~"
        && let Some(home) = dirs_next::home_dir()
    {
        return home.to_string_lossy().into_owned();
    }
    path.to_owned()
}

/// Reads transcript JSONL file and returns the latest assistant message with usage data.
///
/// Transcripts contain conversation history as line-delimited JSON. We look for the
/// most recent message with `type="assistant"` that contains usage metrics.
///
/// # Format
/// Each line is a JSON object:
/// ```json
/// {
///   "type": "assistant",
///   "message": {
///     "model": "claude-sonnet-4-5-20250929",
///     "usage": {...},
///     "stop_reason": "tool_use"
///   },
///   "timestamp": "2025-11-14T05:12:50.346Z"
/// }
/// ```
///
/// # Returns
/// - `Some(Json)` if a valid assistant message with usage is found
/// - `None` if file doesn't exist, can't be read, or no valid messages found
///
/// # Performance
/// Reads entire file sequentially. Typically < 1MB for most sessions.
/// Happens during batching window (200ms), not on critical path.
fn read_latest_assistant_message(transcript_path: &str) -> Option<Json> {
    let expanded_path = expand_path(transcript_path);
    let file = File::open(Path::new(&expanded_path)).ok()?;
    let reader = BufReader::new(file);

    let mut latest: Option<Json> = None;

    for line in reader.lines() {
        // Skip lines with I/O errors instead of aborting the scan
        let Ok(line) = line else { continue };

        if let Ok(entry) = serde_json::from_str::<Json>(&line)
            && entry.get("type").and_then(|t| t.as_str()) == Some("assistant")
            && let Some(msg) = entry.get("message")
            && msg.get("usage").is_some()
        {
            latest = Some(entry);
        }
    }

    latest
}

/// Enriches payload with data from the latest assistant message.
///
/// Mutates `payload` in-place to add:
/// - `model`: Model identifier
/// - `usage`: Aggregated token counts (prompt_tokens, completion_tokens, total_tokens)
/// - `finish_reason`: Mapped from `stop_reason`
/// - `timestamp`: Event timestamp (only if not already present)
///
/// # Token Aggregation
/// Anthropic API returns separate cache token fields:
/// - `input_tokens`: Fresh prompt tokens
/// - `cache_creation_input_tokens`: Tokens written to cache
/// - `cache_read_input_tokens`: Tokens read from cache
/// - `output_tokens`: Generated tokens
///
/// We aggregate to match OpenAI format:
/// ```
/// prompt_tokens = input_tokens + cache_creation + cache_read
/// completion_tokens = output_tokens
/// total_tokens = prompt_tokens + completion_tokens
/// ```
fn enrich_from_transcript(payload: &mut Json, latest_msg: &Json) {
    let Some(msg) = latest_msg.get("message") else {
        return;
    };

    // Enrich with model
    if let Some(model) = msg.get("model")
        && let Some(obj) = payload.as_object_mut()
    {
        obj.insert("model".to_string(), model.clone());
    }

    // Enrich with aggregated usage
    if let Some(usage) = msg.get("usage") {
        let input_tokens = usage
            .get("input_tokens")
            .and_then(|t| t.as_u64())
            .unwrap_or(0);
        let cache_creation = usage
            .get("cache_creation_input_tokens")
            .and_then(|t| t.as_u64())
            .unwrap_or(0);
        let cache_read = usage
            .get("cache_read_input_tokens")
            .and_then(|t| t.as_u64())
            .unwrap_or(0);
        let output_tokens = usage
            .get("output_tokens")
            .and_then(|t| t.as_u64())
            .unwrap_or(0);

        let enriched_usage = serde_json::json!({
            "prompt_tokens": input_tokens + cache_creation + cache_read,
            "completion_tokens": output_tokens,
            "total_tokens": input_tokens + cache_creation + cache_read + output_tokens,
            "token_counts_estimated": false
        });

        if let Some(obj) = payload.as_object_mut() {
            obj.insert("usage".to_string(), enriched_usage);
        }
    }

    // Enrich with finish_reason (from stop_reason)
    if let Some(stop_reason) = msg.get("stop_reason")
        && let Some(obj) = payload.as_object_mut()
    {
        obj.insert("finish_reason".to_string(), stop_reason.clone());
    }

    // Enrich with timestamp if not already present
    if payload.get("timestamp").is_none()
        && let Some(timestamp) = latest_msg.get("timestamp")
        && let Some(obj) = payload.as_object_mut()
    {
        obj.insert("timestamp".to_string(), timestamp.clone());
    }
}

/// Transforms a tap frame JSON payload into a TraceV1 telemetry event.
///
/// **Fast Path**: If `schema_version` and `ids` fields are present, deserializes
/// directly as a pre-formed TraceV1 event.
///
/// **Fallback Path**: Extracts fields from legacy tap wrapper format:
/// - `event`: Event type (normalized via `normalize_event`)
/// - `ts`: ISO timestamp
/// - `env`: Environment metadata (host, pid, session_id)
/// - `payload`: Nested telemetry data (model config, tool usage, metrics)
/// - `plugin`, `version`: Plugin identity
///
/// Applies defaults for missing fields and preserves the original payload in
/// `extensions["tap.raw"]` for audit purposes.
///
/// # Errors
///
/// Returns an error if fast path deserialization fails due to invalid TraceV1 structure.
pub fn from_tap_frame(v: Json) -> Result<TraceV1> {
    // Fast path: Accept pre-formed TraceV1 events from newer plugins.
    if v.get("schema_version").is_some() && v.get("ids").is_some() {
        return serde_json::from_value::<TraceV1>(v).map_err(|e| anyhow!("TraceV1 parse: {e}"));
    }

    // Fallback path: Extract from legacy tap wrapper format.
    let event = v
        .get("event")
        .and_then(|x| x.as_str())
        .unwrap_or("unknown")
        .to_string();
    let ts = v
        .get("ts")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let env = v
        .get("env")
        .cloned()
        .unwrap_or_else(|| Json::Object(Default::default()));
    let mut payload = v
        .get("payload")
        .cloned()
        .unwrap_or_else(|| Json::Object(Default::default()));

    // Clone raw payload before enrichment for audit trail
    let raw_payload_for_audit = payload.clone();

    // Enrich from transcript if path is present and store latest message for conversation_id extraction
    let latest_msg = if let Some(transcript_path) = payload.get("transcript_path").and_then(|p| p.as_str()) {
        let msg = read_latest_assistant_message(transcript_path);
        if let Some(ref m) = msg {
            enrich_from_transcript(&mut payload, m);
        }
        msg
    } else {
        None
    };

    // Set timestamp with cascading fallback: tap frame ts → enriched payload → empty
    let timestamp = if !ts.is_empty() {
        ts
    } else if let Some(ts) = payload.get("timestamp").and_then(|x| x.as_str()) {
        ts.to_string()
    } else {
        String::new()
    };

    let mut t = TraceV1 {
        event: normalize_event(&event).to_string(),
        timestamp,
        ..Default::default()
    };

    // Extract context. Default plugin to "beak" for backward compatibility.
    t.context.plugin = v
        .get("plugin")
        .and_then(|x| x.as_str())
        .unwrap_or("beak")
        .to_string();
    t.context.plugin_version = v
        .get("version")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    if let Some(host) = env.get("host").and_then(|x| x.as_str()) {
        t.context.host = host.to_string();
    }
    if let Some(pid) = env.get("pid") {
        t.context.pid = as_u32_sat(pid);
    }
    if let Some(sid) = env.get("session_id").and_then(|x| x.as_str()) {
        t.ids.session_id = sid.to_string();
    }

    // Extract conversation_id from transcript message.id (maps to Claude's message ID)
    if let Some(ref msg) = latest_msg {
        t.ids.conversation_id = msg
            .get("message")
            .and_then(|m| m.get("id"))
            .and_then(|id| id.as_str())
            .unwrap_or("")
            .to_string();
    }

    // Extract model configuration.
    if let Some(m) = payload.get("model").and_then(|x| x.as_str()) {
        t.configuration.model = m.to_string();
    }

    // Extract parameters from payload only.
    // If a parameter isn't captured, we leave it as 0 to indicate missing data.
    t.configuration.temperature = payload
        .get("temperature")
        .and_then(|v| v.as_f64())
        .map(|v| v as f32)
        .unwrap_or(0.0);

    t.configuration.top_p = payload
        .get("top_p")
        .and_then(|v| v.as_f64())
        .map(|v| v as f32)
        .unwrap_or(0.0);

    t.configuration.top_k = payload
        .get("top_k")
        .and_then(|v| v.as_i64())
        .map(|v| v as u32)
        .unwrap_or(0);

    t.configuration.max_tokens = payload
        .get("max_tokens")
        .and_then(|v| v.as_i64())
        .map(|v| v as u32)
        .unwrap_or(0);

    // Extract tool usage details.
    if let Some(name) = payload.get("tool_name").and_then(|x| x.as_str()) {
        t.inputs.tool.name = name.to_string();
    }
    if let Some(ver) = payload.get("tool_version").and_then(|x| x.as_str()) {
        t.inputs.tool.version = ver.to_string();
    }
    if let Some(args) = payload.get("tool_input") {
        t.inputs.tool.args = args.clone();
    }

    // Extract output metadata.
    if let Some(resp) = payload.get("tool_response").and_then(|x| x.as_str()) {
        t.outputs.assistant_text = resp.to_string();
    }
    if let Some(fr) = payload.get("finish_reason").and_then(|x| x.as_str()) {
        t.outputs.finish_reason = fr.to_string();
    }

    // Extract usage metrics into both metrics and outputs (for Beak compatibility).
    if let Some(u) = payload.get("usage") {
        let prompt_tokens = u.get("prompt_tokens").map(as_u32_sat).unwrap_or(0);
        let completion_tokens = u.get("completion_tokens").map(as_u32_sat).unwrap_or(0);
        let total_tokens = u.get("total_tokens").map(as_u32_sat).unwrap_or(0);
        let tokens_estimated = u
            .get("token_counts_estimated")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);

        // Populate metrics object (existing behavior)
        t.metrics.prompt_tokens = prompt_tokens;
        t.metrics.completion_tokens = completion_tokens;
        t.metrics.total_tokens = total_tokens;
        t.metrics.token_counts_estimated = tokens_estimated;

        // Also populate outputs object for Beak compatibility
        t.outputs.input_tokens = prompt_tokens;
        t.outputs.output_tokens = completion_tokens;
        t.outputs.total_tokens = total_tokens;
        t.outputs.tokens_estimated = tokens_estimated;
    }

    // Extract latency metrics.
    if let Some(l) = payload.get("latency_ms") {
        t.metrics.latency_ms.first_token = l.get("first_token").map(as_u32_sat).unwrap_or(0);
        t.metrics.latency_ms.provider = l.get("provider").map(as_u32_sat).unwrap_or(0);
        t.metrics.latency_ms.total = l.get("total").map(as_u32_sat).unwrap_or(0);
    }

    // Extract latency_estimated from canonical location (payload level) or fallback to nested location
    t.metrics.latency_estimated = payload
        .get("latency_estimated")
        .and_then(|x| x.as_bool())
        .or_else(|| {
            payload
                .get("latency_ms")
                .and_then(|l| l.get("latency_estimated"))
                .and_then(|x| x.as_bool())
        })
        .unwrap_or(false);

    // Preserve original tap payload in extensions for audit trail.
    let mut ext = serde_json::Map::new();
    ext.insert("tap.raw".to_string(), raw_payload_for_audit);
    t.extensions = Json::Object(ext);

    Ok(t)
}

/// Normalizes event type strings to canonical form.
///
/// Maps PascalCase (e.g., "PostToolUse") and dotted notation (e.g., "tool.post")
/// to a single canonical format.
///
/// Supported mappings:
/// - `PostToolUse` / `tool.post` → `"tool.post"`
/// - `ModelEnd` / `model.end` → `"model.end"`
/// - `SessionStart` / `session.start` → `"session.start"`
/// - `SessionEnd` / `session.end` → `"session.end"`
/// - Unknown → `"unknown"`
fn normalize_event(e: &str) -> &str {
    match e {
        "PostToolUse" | "tool.post" => "tool.post",
        "ModelEnd" | "model.end" => "model.end",
        "SessionStart" | "session.start" => "session.start",
        "SessionEnd" | "session.end" => "session.end",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_path_with_tilde() {
        let expanded = expand_path("~/foo/bar.txt");
        assert!(expanded.starts_with('/'), "Should be absolute path");
        assert!(
            expanded.contains("foo/bar.txt"),
            "Should preserve path suffix"
        );
        assert!(!expanded.contains('~'), "Should not contain tilde");
    }

    #[test]
    fn test_expand_path_without_tilde() {
        let path = "/absolute/path.txt";
        let expanded = expand_path(path);
        assert_eq!(expanded, path, "Absolute paths should be unchanged");
    }

    #[test]
    fn test_read_latest_assistant_message_basic() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, r#"{{"type":"user","message":{{"role":"user"}}}}"#).unwrap();
        writeln!(
            file,
            r#"{{"type":"assistant","message":{{"model":"claude-sonnet-4-5-20250929","usage":{{"input_tokens":100}}}}}}"#
        )
        .unwrap();
        writeln!(file, r#"{{"type":"user","message":{{"role":"user"}}}}"#).unwrap();
        file.flush().unwrap();

        let result = read_latest_assistant_message(file.path().to_str().unwrap());
        assert!(result.is_some(), "Should find assistant message");

        let msg = result.unwrap();
        assert_eq!(msg.get("type").and_then(|t| t.as_str()), Some("assistant"));
        assert!(msg.get("message").and_then(|m| m.get("usage")).is_some());
    }

    #[test]
    fn test_read_latest_assistant_message_returns_latest() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"{{"type":"assistant","message":{{"model":"old-model","usage":{{"input_tokens":100}}}}}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"type":"assistant","message":{{"model":"middle-model","usage":{{"input_tokens":200}}}}}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"type":"assistant","message":{{"model":"latest-model","usage":{{"input_tokens":300}}}}}}"#
        )
        .unwrap();
        file.flush().unwrap();

        let result = read_latest_assistant_message(file.path().to_str().unwrap());
        assert!(result.is_some());

        let msg = result.unwrap();
        let model = msg
            .get("message")
            .and_then(|m| m.get("model"))
            .and_then(|m| m.as_str());
        assert_eq!(model, Some("latest-model"), "Should return latest message");
    }

    #[test]
    fn test_read_latest_assistant_message_skips_without_usage() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"{{"type":"assistant","message":{{"model":"no-usage-model"}}}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"type":"assistant","message":{{"model":"with-usage","usage":{{"input_tokens":100}}}}}}"#
        )
        .unwrap();
        file.flush().unwrap();

        let result = read_latest_assistant_message(file.path().to_str().unwrap());
        assert!(result.is_some());

        let msg = result.unwrap();
        let model = msg
            .get("message")
            .and_then(|m| m.get("model"))
            .and_then(|m| m.as_str());
        assert_eq!(model, Some("with-usage"));
    }

    #[test]
    fn test_read_latest_assistant_message_file_not_found() {
        let result = read_latest_assistant_message("/nonexistent/path.jsonl");
        assert!(result.is_none(), "Should return None for missing file");
    }

    #[test]
    fn test_enrich_from_transcript_adds_model_and_usage() {
        let mut payload = serde_json::json!({});
        let transcript_msg = serde_json::json!({
            "message": {
                "model": "claude-sonnet-4-5-20250929",
                "usage": {
                    "input_tokens": 1000,
                    "cache_creation_input_tokens": 500,
                    "cache_read_input_tokens": 2000,
                    "output_tokens": 150
                },
                "stop_reason": "tool_use"
            }
        });

        enrich_from_transcript(&mut payload, &transcript_msg);

        assert_eq!(
            payload.get("model").and_then(|m| m.as_str()),
            Some("claude-sonnet-4-5-20250929")
        );

        let usage = payload.get("usage").expect("usage should be present");
        assert_eq!(
            usage.get("prompt_tokens").and_then(|t| t.as_u64()),
            Some(3500)
        ); // 1000+500+2000
        assert_eq!(
            usage.get("completion_tokens").and_then(|t| t.as_u64()),
            Some(150)
        );
        assert_eq!(
            usage.get("total_tokens").and_then(|t| t.as_u64()),
            Some(3650)
        ); // 3500+150
        assert_eq!(
            usage
                .get("token_counts_estimated")
                .and_then(|e| e.as_bool()),
            Some(false)
        );
    }

    #[test]
    fn test_enrich_from_transcript_preserves_existing_timestamp() {
        let original_ts = "2025-01-01T00:00:00Z";
        let mut payload = serde_json::json!({
            "timestamp": original_ts
        });

        let transcript_msg = serde_json::json!({
            "message": {},
            "timestamp": "2025-12-31T23:59:59Z"
        });

        enrich_from_transcript(&mut payload, &transcript_msg);

        assert_eq!(
            payload.get("timestamp").and_then(|t| t.as_str()),
            Some(original_ts),
            "Should preserve existing timestamp"
        );
    }

    #[test]
    fn test_from_tap_frame_with_transcript_enrichment() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        // Create transcript file
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"{{"type":"assistant","message":{{"model":"claude-sonnet-4-5-20250929","usage":{{"input_tokens":1000,"cache_creation_input_tokens":500,"cache_read_input_tokens":2000,"output_tokens":150}},"stop_reason":"tool_use"}},"timestamp":"2025-11-14T05:12:50.346Z"}}"#
        )
        .unwrap();
        file.flush().unwrap();

        // Create tap frame with transcript_path
        let frame = serde_json::json!({
            "event": "PostToolUse",
            "ts": "2025-11-13T10:30:00Z",
            "env": {
                "host": "test-host",
                "pid": 1234,
                "session_id": "test-session"
            },
            "payload": {
                "transcript_path": file.path().to_str().unwrap(),
                "tool_name": "Bash"
            },
            "plugin": "test-plugin",
            "version": "1.0.0"
        });

        let result = from_tap_frame(frame);
        assert!(result.is_ok(), "from_tap_frame should succeed");

        let trace = result.unwrap();
        assert_eq!(trace.configuration.model, "claude-sonnet-4-5-20250929");
        assert_eq!(trace.metrics.prompt_tokens, 3500); // 1000+500+2000
        assert_eq!(trace.metrics.completion_tokens, 150);
        assert_eq!(trace.metrics.total_tokens, 3650);
        assert_eq!(trace.outputs.finish_reason, "tool_use");
    }

    #[test]
    fn test_token_duplication_in_outputs() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        // Create transcript file with usage data
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"{{"type":"assistant","message":{{"id":"msg_abc123","model":"claude-sonnet-4-5-20250929","usage":{{"input_tokens":1000,"cache_creation_input_tokens":500,"cache_read_input_tokens":2000,"output_tokens":150}},"stop_reason":"end_turn"}},"timestamp":"2025-11-14T05:12:50.346Z"}}"#
        )
        .unwrap();
        file.flush().unwrap();

        // Create tap frame with transcript_path
        let frame = serde_json::json!({
            "event": "model.end",
            "ts": "2025-11-13T10:30:00Z",
            "env": {
                "host": "test-host",
                "pid": 1234,
                "session_id": "test-session"
            },
            "payload": {
                "transcript_path": file.path().to_str().unwrap()
            },
            "plugin": "talon",
            "version": "0.1.0"
        });

        let result = from_tap_frame(frame);
        assert!(result.is_ok(), "from_tap_frame should succeed");

        let trace = result.unwrap();

        // Verify tokens are in metrics object (existing behavior)
        assert_eq!(trace.metrics.prompt_tokens, 3500); // 1000+500+2000
        assert_eq!(trace.metrics.completion_tokens, 150);
        assert_eq!(trace.metrics.total_tokens, 3650);
        assert_eq!(trace.metrics.token_counts_estimated, false);

        // Verify tokens are ALSO in outputs object (new behavior for Beak compatibility)
        assert_eq!(trace.outputs.input_tokens, 3500);
        assert_eq!(trace.outputs.output_tokens, 150);
        assert_eq!(trace.outputs.total_tokens, 3650);
        assert_eq!(trace.outputs.tokens_estimated, false);

        // Verify conversation_id is extracted from message.id
        assert_eq!(trace.ids.conversation_id, "msg_abc123");
    }

}
