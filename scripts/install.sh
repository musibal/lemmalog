#!/usr/bin/env bash
# lemmalog installer: builds the binaries, registers the MCP server with
# every supported agent CLI it finds, and installs the skill.
#
#   ./scripts/install.sh              # install
#   ./scripts/install.sh --uninstall  # remove registrations + skill
#
# Env: LEMMALOG_SNAPSHOT (default ~/.lemmalog/memory.snap) — where memory
# persists across sessions.

set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="$(pwd)"
BIN="$ROOT/target/release"
SNAP="${LEMMALOG_SNAPSHOT:-$HOME/.lemmalog/memory.snap}"
SKILL_SRC="$ROOT/skills/lemmalog"
MODE="install"
[ "${1:-}" = "--uninstall" ] && MODE="uninstall"

have() { command -v "$1" >/dev/null 2>&1; }

build() {
    echo "==> building lemmalog (release)..."
    cargo build --release --features mcp
}

register_claude() {
    if have claude; then
        if [ "$MODE" = "install" ]; then
            echo "==> registering MCP server with Claude Code"
            claude mcp add lemmalog \
                --env LEMMALOG_MCP_PATH="$SNAP" \
                -- "$BIN/lemmalog-mcp" 2>/dev/null \
              || claude mcp remove lemmalog >/dev/null 2>&1 && \
                 claude mcp add lemmalog \
                    --env LEMMALOG_MCP_PATH="$SNAP" \
                    -- "$BIN/lemmalog-mcp"
            mkdir -p ~/.claude/skills
            rm -rf ~/.claude/skills/lemmalog
            cp -r "$SKILL_SRC" ~/.claude/skills/lemmalog
            echo "    skill -> ~/.claude/skills/lemmalog"
        else
            echo "==> removing lemmalog from Claude Code"
            claude mcp remove lemmalog >/dev/null 2>&1 || true
            rm -rf ~/.claude/skills/lemmalog
        fi
    else
        echo "-- claude CLI not found, skipping"
    fi
}

register_kimi() {
    if have kimi; then
        if [ "$MODE" = "install" ]; then
            echo "==> registering MCP server with Kimi CLI"
            kimi mcp add lemmalog \
                --env LEMMALOG_MCP_PATH="$SNAP" \
                -- "$BIN/lemmalog-mcp" 2>/dev/null \
              || { kimi mcp remove lemmalog >/dev/null 2>&1 || true; \
                   kimi mcp add lemmalog \
                    --env LEMMALOG_MCP_PATH="$SNAP" \
                    -- "$BIN/lemmalog-mcp"; }
            mkdir -p ~/.kimi/skills 2>/dev/null || true
            if [ -d ~/.kimi ]; then
                rm -rf ~/.kimi/skills/lemmalog
                cp -r "$SKILL_SRC" ~/.kimi/skills/lemmalog
                echo "    skill -> ~/.kimi/skills/lemmalog"
            fi
        else
            echo "==> removing lemmalog from Kimi CLI"
            kimi mcp remove lemmalog >/dev/null 2>&1 || true
            rm -rf ~/.kimi/skills/lemmalog 2>/dev/null || true
        fi
    else
        echo "-- kimi CLI not found, skipping"
    fi
}

if [ "$MODE" = "install" ]; then
    build
    mkdir -p "$(dirname "$SNAP")"
    echo "==> memory persists at $SNAP"
else
    echo "==> uninstalling lemmalog registrations"
fi

register_claude
register_kimi

if [ "$MODE" = "install" ]; then
    cat <<EOF

done. verify with:
  claude mcp list  (or: kimi mcp list)

then in a session, look for the lemmalog_* tools. the snapshot at
$SNAP carries memory across sessions and restarts.
EOF
else
    echo
    echo "done. (binaries and $SNAP left in place — delete manually if wanted)"
fi
