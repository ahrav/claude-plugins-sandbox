//! Transforms TraceV1 events into Beak-compatible format.
//!
//! The critical transformation is moving token metrics from `TraceV1.metrics` into
//! `outputs`, as Beak's UI expects to find `input_tokens`, `output_tokens`, and
//! `total_tokens` inside outputs for visualization.

use crate::schema::TraceV1;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

/// Beak-compatible trace structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeakTrace {
    /// Shortened trace identifier (first 8 characters).
    pub id: String,

    pub timestamp: String,

    /// Collection name (defaults to "claude-code").
    pub collection: String,

    /// Flow identifier (defaults to "conversations").
    pub flow: String,

    /// Input data including model context, session, and messages.
    pub inputs: Json,

    /// Output data with token metrics, response, latency, and cost.
    /// Token metrics must be at top level for Beak UI.
    pub outputs: Json,

    /// Model and sampling parameters.
    pub configuration: Json,

    #[serde(default)]
    pub labels: Vec<Label>,

    #[serde(default)]
    pub files: Vec<String>,
}

/// Label key-value pair for filtering and grouping.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Label {
    pub key: String,
    pub value: String,
}

/// Transforms a TraceV1 event into Beak-compatible format.
///
/// Key transformations:
/// - Shortens trace_id to first 8 characters
/// - Moves token metrics from `trace.metrics` into `outputs` (required by Beak UI)
/// - Constructs inputs, outputs, and configuration JSONB objects
pub fn to_beak_format(trace: &TraceV1) -> BeakTrace {
    let id = if trace.ids.trace_id.len() >= 8 {
        trace.ids.trace_id[..8].to_string()
    } else {
        trace.ids.trace_id.clone()
    };

    let inputs = serde_json::json!({
        "model": trace.configuration.model,
        "session_id": trace.ids.session_id,
        "conversation_id": trace.ids.conversation_id,
        "tool_name": trace.inputs.tool.name,
        "tool_version": trace.inputs.tool.version,
        "tool_args": trace.inputs.tool.args,
        "messages": trace.inputs.messages_compact.iter().map(|msg| {
            serde_json::json!({
                "role": msg.role,
                "content": msg.content
            })
        }).collect::<Vec<_>>(),
        "retrieval_items": trace.inputs.retrieval_items.iter().map(|item| {
            serde_json::json!({
                "id": item.id,
                "type": item.r#type,
                "score": item.score,
                "size_tokens": item.size_tokens
            })
        }).collect::<Vec<_>>()
    });

    let outputs = serde_json::json!({
        "response": trace.outputs.assistant_text,
        "finish_reason": trace.outputs.finish_reason,
        "truncated": trace.outputs.truncated,
        "tool_calls": trace.outputs.tool_calls.iter().map(|tc| {
            serde_json::json!({
                "name": tc.name,
                "args": tc.args,
                "status": tc.status
            })
        }).collect::<Vec<_>>(),
        "input_tokens": trace.metrics.prompt_tokens,
        "output_tokens": trace.metrics.completion_tokens,
        "total_tokens": trace.metrics.total_tokens,
        "tokens_estimated": trace.metrics.token_counts_estimated,
        "latency_ms": {
            "first_token": trace.metrics.latency_ms.first_token,
            "provider": trace.metrics.latency_ms.provider,
            "total": trace.metrics.latency_ms.total
        },
        "latency_estimated": trace.metrics.latency_estimated,
        "input_cost_usd": trace.metrics.input_cost_usd,
        "output_cost_usd": trace.metrics.output_cost_usd,
        "total_cost_usd": trace.metrics.total_cost_usd,
        "quality_score": trace.metrics.quality_score
    });

    let configuration = serde_json::json!({
        "model": trace.configuration.model,
        "temperature": trace.configuration.temperature,
        "top_p": trace.configuration.top_p,
        "top_k": trace.configuration.top_k,
        "max_tokens": trace.configuration.max_tokens,
        "seed": trace.configuration.seed,
        "stop_sequences": trace.configuration.stop_sequences
    });

    let labels = trace
        .labels
        .iter()
        .map(|label| Label {
            key: label.key.clone(),
            value: label.value.clone(),
        })
        .collect();

    BeakTrace {
        id,
        timestamp: trace.timestamp.clone(),
        collection: "claude-code".to_string(),
        flow: "conversations".to_string(),
        inputs,
        outputs,
        configuration,
        labels,
        files: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::*;

    #[test]
    fn test_to_beak_format_basic() {
        let mut trace = TraceV1::default();
        trace.ids.trace_id = "12345678-1234-1234-1234-123456789abc".to_string();
        trace.timestamp = "2025-11-13T10:30:00Z".to_string();
        trace.configuration.model = "claude-sonnet-4-5-20250929".to_string();
        trace.metrics.prompt_tokens = 1000;
        trace.metrics.completion_tokens = 150;
        trace.metrics.total_tokens = 1150;

        let beak = to_beak_format(&trace);

        assert_eq!(beak.id, "12345678");
        assert_eq!(beak.timestamp, "2025-11-13T10:30:00Z");
        assert_eq!(beak.collection, "claude-code");
        assert_eq!(beak.flow, "conversations");
    }

    #[test]
    fn test_token_metrics_in_outputs() {
        let mut trace = TraceV1::default();
        trace.metrics.prompt_tokens = 1000;
        trace.metrics.completion_tokens = 150;
        trace.metrics.total_tokens = 1150;
        trace.metrics.token_counts_estimated = true;

        let beak = to_beak_format(&trace);

        // Verify token metrics are in outputs
        assert_eq!(
            beak.outputs.get("input_tokens").and_then(|v| v.as_u64()),
            Some(1000)
        );
        assert_eq!(
            beak.outputs.get("output_tokens").and_then(|v| v.as_u64()),
            Some(150)
        );
        assert_eq!(
            beak.outputs.get("total_tokens").and_then(|v| v.as_u64()),
            Some(1150)
        );
        assert_eq!(
            beak.outputs
                .get("tokens_estimated")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn test_configuration_mapping() {
        let mut trace = TraceV1::default();
        trace.configuration.model = "claude-3-opus-20240229".to_string();
        trace.configuration.temperature = 0.7;
        trace.configuration.top_p = 0.9;
        trace.configuration.top_k = 40;
        trace.configuration.max_tokens = 4096;
        trace.configuration.seed = 42;

        let beak = to_beak_format(&trace);

        assert_eq!(
            beak.configuration.get("model").and_then(|v| v.as_str()),
            Some("claude-3-opus-20240229")
        );

        // Use approximate comparison for f32 values
        let temp = beak
            .configuration
            .get("temperature")
            .and_then(|v| v.as_f64())
            .unwrap();
        assert!((temp - 0.7).abs() < 0.01);

        let top_p = beak
            .configuration
            .get("top_p")
            .and_then(|v| v.as_f64())
            .unwrap();
        assert!((top_p - 0.9).abs() < 0.01);

        assert_eq!(
            beak.configuration.get("top_k").and_then(|v| v.as_u64()),
            Some(40)
        );
        assert_eq!(
            beak.configuration
                .get("max_tokens")
                .and_then(|v| v.as_u64()),
            Some(4096)
        );
        assert_eq!(
            beak.configuration.get("seed").and_then(|v| v.as_u64()),
            Some(42)
        );
    }

    #[test]
    fn test_inputs_mapping() {
        let mut trace = TraceV1::default();
        trace.configuration.model = "claude-sonnet-4-5-20250929".to_string();
        trace.ids.session_id = "session-123".to_string();
        trace.ids.conversation_id = "conv-456".to_string();
        trace.inputs.tool.name = "Bash".to_string();
        trace.inputs.tool.version = "1.0".to_string();
        trace.inputs.messages_compact.push(Message {
            role: "user".to_string(),
            content: "Hello".to_string(),
        });

        let beak = to_beak_format(&trace);

        assert_eq!(
            beak.inputs.get("model").and_then(|v| v.as_str()),
            Some("claude-sonnet-4-5-20250929")
        );
        assert_eq!(
            beak.inputs.get("session_id").and_then(|v| v.as_str()),
            Some("session-123")
        );
        assert_eq!(
            beak.inputs.get("conversation_id").and_then(|v| v.as_str()),
            Some("conv-456")
        );
        assert_eq!(
            beak.inputs.get("tool_name").and_then(|v| v.as_str()),
            Some("Bash")
        );

        let messages = beak.inputs.get("messages").and_then(|v| v.as_array());
        assert!(messages.is_some());
        assert_eq!(messages.unwrap().len(), 1);
    }

    #[test]
    fn test_outputs_mapping() {
        let mut trace = TraceV1::default();
        trace.outputs.assistant_text = "Response text".to_string();
        trace.outputs.finish_reason = "stop".to_string();
        trace.outputs.truncated = false;
        trace.metrics.latency_ms.total = 1500;
        trace.metrics.input_cost_usd = 0.01;
        trace.metrics.output_cost_usd = 0.02;
        trace.metrics.total_cost_usd = 0.03;

        let beak = to_beak_format(&trace);

        assert_eq!(
            beak.outputs.get("response").and_then(|v| v.as_str()),
            Some("Response text")
        );
        assert_eq!(
            beak.outputs.get("finish_reason").and_then(|v| v.as_str()),
            Some("stop")
        );
        assert_eq!(
            beak.outputs.get("truncated").and_then(|v| v.as_bool()),
            Some(false)
        );
        assert_eq!(
            beak.outputs
                .get("latency_ms")
                .and_then(|l| l.get("total"))
                .and_then(|v| v.as_u64()),
            Some(1500)
        );

        // Use approximate comparison for f32 values
        let total_cost = beak
            .outputs
            .get("total_cost_usd")
            .and_then(|v| v.as_f64())
            .unwrap();
        assert!((total_cost - 0.03).abs() < 0.001);
    }

    #[test]
    fn test_labels_mapping() {
        let mut trace = TraceV1::default();
        trace.labels.push(crate::schema::Label {
            key: "environment".to_string(),
            value: "production".to_string(),
        });
        trace.labels.push(crate::schema::Label {
            key: "team".to_string(),
            value: "ml-team".to_string(),
        });

        let beak = to_beak_format(&trace);

        assert_eq!(beak.labels.len(), 2);
        assert_eq!(beak.labels[0].key, "environment");
        assert_eq!(beak.labels[0].value, "production");
        assert_eq!(beak.labels[1].key, "team");
        assert_eq!(beak.labels[1].value, "ml-team");
    }

    #[test]
    fn test_short_trace_id() {
        let mut trace = TraceV1::default();
        trace.ids.trace_id = "short".to_string();

        let beak = to_beak_format(&trace);

        assert_eq!(beak.id, "short");
    }

    #[test]
    fn test_empty_trace() {
        let trace = TraceV1::default();
        let beak = to_beak_format(&trace);

        assert_eq!(beak.id, "");
        assert_eq!(beak.collection, "claude-code");
        assert_eq!(beak.flow, "conversations");
        assert!(beak.files.is_empty());
    }

    #[test]
    fn test_latency_metrics_in_outputs() {
        let mut trace = TraceV1::default();
        trace.metrics.latency_ms.first_token = 100;
        trace.metrics.latency_ms.provider = 800;
        trace.metrics.latency_ms.total = 1000;
        trace.metrics.latency_estimated = true;

        let beak = to_beak_format(&trace);

        let latency = beak.outputs.get("latency_ms").expect("latency_ms exists");
        assert_eq!(
            latency.get("first_token").and_then(|v| v.as_u64()),
            Some(100)
        );
        assert_eq!(latency.get("provider").and_then(|v| v.as_u64()), Some(800));
        assert_eq!(latency.get("total").and_then(|v| v.as_u64()), Some(1000));
        assert_eq!(
            beak.outputs
                .get("latency_estimated")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn test_cost_metrics_in_outputs() {
        let mut trace = TraceV1::default();
        trace.metrics.input_cost_usd = 0.005;
        trace.metrics.output_cost_usd = 0.015;
        trace.metrics.total_cost_usd = 0.020;
        trace.metrics.quality_score = 0.95;

        let beak = to_beak_format(&trace);

        // Use approximate comparisons for f32 values
        let input_cost = beak
            .outputs
            .get("input_cost_usd")
            .and_then(|v| v.as_f64())
            .unwrap();
        assert!((input_cost - 0.005).abs() < 0.001);

        let output_cost = beak
            .outputs
            .get("output_cost_usd")
            .and_then(|v| v.as_f64())
            .unwrap();
        assert!((output_cost - 0.015).abs() < 0.001);

        let total_cost = beak
            .outputs
            .get("total_cost_usd")
            .and_then(|v| v.as_f64())
            .unwrap();
        assert!((total_cost - 0.020).abs() < 0.001);

        let quality = beak
            .outputs
            .get("quality_score")
            .and_then(|v| v.as_f64())
            .unwrap();
        assert!((quality - 0.95).abs() < 0.01);
    }

    #[test]
    fn test_tool_calls_in_outputs() {
        let mut trace = TraceV1::default();
        trace.outputs.tool_calls.push(ToolCall {
            name: "search".to_string(),
            args: serde_json::json!({"query": "rust"}),
            status: "success".to_string(),
        });

        let beak = to_beak_format(&trace);

        let tool_calls = beak
            .outputs
            .get("tool_calls")
            .and_then(|v| v.as_array())
            .expect("tool_calls array exists");
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(
            tool_calls[0].get("name").and_then(|v| v.as_str()),
            Some("search")
        );
        assert_eq!(
            tool_calls[0].get("status").and_then(|v| v.as_str()),
            Some("success")
        );
    }

    #[test]
    fn test_retrieval_items_in_inputs() {
        let mut trace = TraceV1::default();
        trace.inputs.retrieval_items.push(RetrievalItem {
            id: "doc-123".to_string(),
            r#type: "document".to_string(),
            score: 0.95,
            size_tokens: 500,
        });

        let beak = to_beak_format(&trace);

        let items = beak
            .inputs
            .get("retrieval_items")
            .and_then(|v| v.as_array())
            .expect("retrieval_items array exists");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].get("id").and_then(|v| v.as_str()), Some("doc-123"));

        // Use approximate comparison for f32 value
        let score = items[0].get("score").and_then(|v| v.as_f64()).unwrap();
        assert!((score - 0.95).abs() < 0.01);
    }
}
