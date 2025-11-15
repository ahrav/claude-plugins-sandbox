#!/usr/bin/env bash
#
# Forward Claude Code hook events to talon-tap
#
# This script is invoked by Claude Code hooks and receives event data via stdin.
# It forwards the event to talon-tap, which batches and sends to talon-agent.
#
# Usage:
#   forward-to-talon.sh <event_type>
#
# Arguments:
#   event_type - The hook event type (e.g., "PostToolUse", "Stop")
#
# Environment Variables:
#   TALON_TAP_PATH - Optional path to talon-tap binary (defaults to PATH lookup)
#   TALON_SOCK     - Optional Unix socket path (defaults to /tmp/talon.sock)
#   TRACE_ENDPOINT - Trace collector endpoint (e.g., https://api.example.com/v1/traces)
#   TRACE_API_KEY  - API key for trace collector authentication

set -euo pipefail

# Get event type from first argument
EVENT_TYPE="${1:-unknown}"

# Locate talon-tap binary
TALON_TAP="${TALON_TAP_PATH:-talon-tap}"

# Check if talon-tap is available
if ! command -v "$TALON_TAP" &> /dev/null; then
    echo "Error: talon-tap not found in PATH. Please install or set TALON_TAP_PATH." >&2
    exit 1
fi

# Read stdin and pipe to talon-tap
# The --event flag tells talon-tap what type of event this is
# talon-tap will handle auto-starting the agent if needed
cat | "$TALON_TAP" --event "$EVENT_TYPE"

# Exit code 0 indicates success
# Non-zero exit codes will be treated as errors by Claude Code
exit 0
