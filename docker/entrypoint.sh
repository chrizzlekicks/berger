#!/usr/bin/env bash
set -euo pipefail

# Optional: seed Claude Code login from the host without touching berger's
# own hook config in ~/.claude/settings.json (berger init writes that below).
if [ -f /run/secrets/claude-credentials.json ]; then
  mkdir -p "$HOME/.claude"
  cp /run/secrets/claude-credentials.json "$HOME/.claude/.credentials.json"
  chmod 600 "$HOME/.claude/.credentials.json"
fi

berger init

TMUX_CONF="$HOME/.config/berger/tmux.conf"
if ! grep -qsF "$TMUX_CONF" "$HOME/.tmux.conf" 2>/dev/null; then
  echo "source-file $TMUX_CONF" >> "$HOME/.tmux.conf"
fi

exec "$@"
