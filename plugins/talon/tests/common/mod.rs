//! Common test utilities and helpers for talon tests

use std::path::PathBuf;
use tempfile::TempDir;

/// Test environment that provides isolated temporary directories
/// and automatic cleanup on drop.
pub struct TestEnv {
    pub temp_dir: TempDir,
    pub spool_dir: PathBuf,
}

impl TestEnv {
    /// Create a new test environment with isolated spool directory
    pub fn new() -> Self {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let spool_dir = temp_dir.path().join("spool");
        std::fs::create_dir_all(&spool_dir).expect("failed to create spool dir");

        Self { temp_dir, spool_dir }
    }

    /// Get path to the events.jsonl spool file
    pub fn events_file(&self) -> PathBuf {
        self.spool_dir.join("events.jsonl")
    }

    /// Read all lines from the events.jsonl file
    pub fn read_events(&self) -> Vec<String> {
        let path = self.events_file();
        if !path.exists() {
            return Vec::new();
        }

        std::fs::read_to_string(path)
            .expect("failed to read events file")
            .lines()
            .map(String::from)
            .collect()
    }

    /// Count number of events in spool file
    pub fn event_count(&self) -> usize {
        self.read_events().len()
    }

    /// Get size of events.jsonl file in bytes
    pub fn file_size(&self) -> u64 {
        let path = self.events_file();
        if !path.exists() {
            return 0;
        }

        std::fs::metadata(path)
            .expect("failed to get file metadata")
            .len()
    }
}

impl Default for TestEnv {
    fn default() -> Self {
        Self::new()
    }
}

/// Create a test JSON event
pub fn test_event(id: usize) -> serde_json::Value {
    serde_json::json!({
        "event": "tool.post",
        "id": id,
        "timestamp": "2025-11-13T00:00:00Z",
        "data": format!("test data {}", id)
    })
}

/// Create a test JSON event with specific size (approximately)
pub fn test_event_with_size(id: usize, target_bytes: usize) -> serde_json::Value {
    let padding = "x".repeat(target_bytes.saturating_sub(100));
    serde_json::json!({
        "event": "tool.post",
        "id": id,
        "timestamp": "2025-11-13T00:00:00Z",
        "padding": padding
    })
}

/// Create multiple test events
pub fn test_events(count: usize) -> Vec<serde_json::Value> {
    (0..count).map(test_event).collect()
}
