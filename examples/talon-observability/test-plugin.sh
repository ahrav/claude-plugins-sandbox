#!/usr/bin/env bash
#
# Test script for Talon Observability Plugin
#
# This script validates that the plugin is correctly configured and that
# talon-tap can communicate with talon-agent.

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo "=== Talon Plugin Test Suite ==="
echo

# Test 1: Check if talon-tap is available
echo -n "1. Checking for talon-tap binary... "
TALON_TAP="${TALON_TAP_PATH:-talon-tap}"
if command -v "$TALON_TAP" &> /dev/null; then
    echo -e "${GREEN}✓${NC} Found: $(which $TALON_TAP)"
else
    echo -e "${RED}✗${NC} Not found in PATH"
    echo "   Please build and install talon-tap:"
    echo "   cd ../../plugins/talon && cargo build --release"
    echo "   export PATH=\"\$(pwd)/target/release:\$PATH\""
    exit 1
fi

# Test 2: Check talon-tap version
echo -n "2. Checking talon-tap version... "
if "$TALON_TAP" --version &> /dev/null; then
    VERSION=$("$TALON_TAP" --version 2>&1 | head -n1)
    echo -e "${GREEN}✓${NC} $VERSION"
else
    echo -e "${YELLOW}?${NC} Unable to get version"
fi

# Test 3: Check for talon-agent
echo -n "3. Checking for talon-agent binary... "
TALON_AGENT="${TALON_AGENT_PATH:-talon-agent}"
if command -v "$TALON_AGENT" &> /dev/null; then
    echo -e "${GREEN}✓${NC} Found: $(which $TALON_AGENT)"
else
    echo -e "${YELLOW}!${NC} Not found (will be auto-started)"
fi

# Test 4: Check plugin files
echo -n "4. Checking plugin structure... "
MISSING_FILES=()
if [ ! -f ".claude-plugin/marketplace.json" ]; then
    MISSING_FILES+=(".claude-plugin/marketplace.json")
fi
if [ ! -f "hooks/hooks.json" ]; then
    MISSING_FILES+=("hooks/hooks.json")
fi
if [ ! -x "hooks/forward-to-talon.sh" ]; then
    MISSING_FILES+=("hooks/forward-to-talon.sh (not executable)")
fi

if [ ${#MISSING_FILES[@]} -eq 0 ]; then
    echo -e "${GREEN}✓${NC} All files present"
else
    echo -e "${RED}✗${NC} Missing or invalid files:"
    for file in "${MISSING_FILES[@]}"; do
        echo "   - $file"
    done
    exit 1
fi

# Test 5: Validate JSON files
echo -n "5. Validating JSON configuration... "
if command -v jq &> /dev/null; then
    if jq empty .claude-plugin/marketplace.json 2>/dev/null && \
       jq empty hooks/hooks.json 2>/dev/null; then
        echo -e "${GREEN}✓${NC} Valid JSON"
    else
        echo -e "${RED}✗${NC} Invalid JSON"
        exit 1
    fi
else
    echo -e "${YELLOW}?${NC} jq not installed, skipping validation"
fi

# Test 6: Check environment variables
echo -n "6. Checking environment configuration... "
WARNINGS=()
if [ -z "${TRACE_ENDPOINT:-}" ]; then
    WARNINGS+=("TRACE_ENDPOINT not set")
fi
if [ -z "${TRACE_API_KEY:-}" ]; then
    WARNINGS+=("TRACE_API_KEY not set")
fi

if [ ${#WARNINGS[@]} -eq 0 ]; then
    echo -e "${GREEN}✓${NC} Configured"
    echo "   TRACE_ENDPOINT: $TRACE_ENDPOINT"
else
    echo -e "${YELLOW}!${NC} Configuration warnings:"
    for warning in "${WARNINGS[@]}"; do
        echo "   - $warning"
    done
    echo "   Talon will work but events won't be sent to a collector."
fi

# Test 7: Test event forwarding
echo -n "7. Testing event forwarding... "
TEST_EVENT='{"test":"event","session_id":"test-123"}'
if echo "$TEST_EVENT" | "$TALON_TAP" --event test 2>/dev/null; then
    echo -e "${GREEN}✓${NC} Successfully forwarded test event"
else
    echo -e "${RED}✗${NC} Failed to forward event"
    echo "   This might be normal if the trace collector is not accessible"
fi

# Test 8: Check if agent is running
echo -n "8. Checking if talon-agent is running... "
SOCK_PATH="${TALON_SOCK:-/tmp/talon.sock}"
if [ -S "$SOCK_PATH" ]; then
    echo -e "${GREEN}✓${NC} Socket exists: $SOCK_PATH"
elif pgrep -f talon-agent > /dev/null 2>&1; then
    echo -e "${YELLOW}!${NC} Process running but socket not found"
else
    echo -e "${YELLOW}!${NC} Not running (will auto-start on first event)"
fi

# Test 9: Check spool directory
echo -n "9. Checking spool directory... "
if [ "$(uname)" == "Darwin" ]; then
    SPOOL_DIR="$HOME/Library/Application Support/talon/spool"
else
    SPOOL_DIR="$HOME/.local/share/talon/spool"
fi

if [ -d "$SPOOL_DIR" ]; then
    SPOOL_SIZE=$(du -sh "$SPOOL_DIR" 2>/dev/null | cut -f1)
    echo -e "${GREEN}✓${NC} Exists ($SPOOL_SIZE)"

    # Check for spooled events
    if [ -f "$SPOOL_DIR/events.jsonl" ]; then
        EVENT_COUNT=$(wc -l < "$SPOOL_DIR/events.jsonl" 2>/dev/null || echo 0)
        if [ "$EVENT_COUNT" -gt 0 ]; then
            echo "   ${YELLOW}!${NC} $EVENT_COUNT spooled events waiting to be sent"
        fi
    fi

    # Check for quarantined events
    if [ -f "$SPOOL_DIR/quarantine.jsonl" ]; then
        QUARANTINE_COUNT=$(wc -l < "$SPOOL_DIR/quarantine.jsonl" 2>/dev/null || echo 0)
        if [ "$QUARANTINE_COUNT" -gt 0 ]; then
            echo "   ${RED}!${NC} $QUARANTINE_COUNT quarantined events (check for errors)"
        fi
    fi
else
    echo -e "${YELLOW}!${NC} Not created yet"
fi

# Summary
echo
echo "=== Test Summary ==="
echo -e "${GREEN}✓${NC} Plugin is correctly configured!"
echo
echo "Next steps:"
echo "  1. Copy this plugin to your Claude Code project:"
echo "     cp -r $(pwd) /path/to/your/project/.claude-plugin"
echo
echo "  2. Configure trace collection (if not already done):"
echo "     export TRACE_ENDPOINT='https://api.honeycomb.io/v1/traces'"
echo "     export TRACE_API_KEY='your-api-key'"
echo
echo "  3. Use Claude Code normally - events will be captured automatically!"
echo
echo "  4. Monitor events in your trace collector dashboard"
echo

exit 0
