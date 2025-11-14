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
    let payload = v
        .get("payload")
        .cloned()
        .unwrap_or_else(|| Json::Object(Default::default()));

    let mut t = TraceV1 {
        event: normalize_event(&event).to_string(),
        timestamp: ts,
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
    if let Some(pid) = env.get("pid").and_then(|x| x.as_u64()) {
        t.context.pid = pid as u32;
    }
    if let Some(sid) = env.get("session_id").and_then(|x| x.as_str()) {
        t.ids.session_id = sid.to_string();
    }

    // Extract model configuration.
    if let Some(m) = payload.get("model").and_then(|x| x.as_str()) {
        t.configuration.model = m.to_string();
    }
    if let Some(temp) = payload.get("temperature").and_then(|x| x.as_f64()) {
        t.configuration.temperature = temp as f32;
    }
    if let Some(tp) = payload.get("top_p").and_then(|x| x.as_f64()) {
        t.configuration.top_p = tp as f32;
    }
    if let Some(mt) = payload.get("max_tokens").and_then(|x| x.as_u64()) {
        t.configuration.max_tokens = mt as u32;
    }

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

    // Extract usage metrics.
    if let Some(u) = payload.get("usage") {
        t.metrics.prompt_tokens =
            u.get("prompt_tokens").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
        t.metrics.completion_tokens = u
            .get("completion_tokens")
            .and_then(|x| x.as_u64())
            .unwrap_or(0) as u32;
        t.metrics.total_tokens = u.get("total_tokens").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
        t.metrics.token_counts_estimated = u
            .get("token_counts_estimated")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
    }

    // Extract latency metrics.
    if let Some(l) = payload.get("latency_ms") {
        t.metrics.latency_ms.first_token =
            l.get("first_token").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
        t.metrics.latency_ms.provider =
            l.get("provider").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
        t.metrics.latency_ms.total = l.get("total").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
        t.metrics.latency_estimated = l
            .get("latency_estimated")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
    }

    // Preserve original tap payload in extensions for audit trail.
    let mut ext = serde_json::Map::new();
    ext.insert("tap.raw".to_string(), payload);
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
