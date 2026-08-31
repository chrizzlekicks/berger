#!/usr/bin/env bash
# Pipe a synthetic Claude Code hook payload into `berger event`, for testing
# window-rename behavior without a live Claude session.
#
# Usage: fake-event <SessionStart|UserPromptSubmit|PreToolUse|PermissionRequest|PostToolUseFailure|Stop|StopFailure|SessionEnd>
set -euo pipefail

event="${1:?usage: fake-event <event-name>}"
session="$(tmux display-message -p '#S' 2>/dev/null || echo test)"

printf '{"hook_event_name":"%s","session_id":"%s"}' "$event" "$session" | berger event
