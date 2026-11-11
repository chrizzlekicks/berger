#!/usr/bin/env bash
set -euo pipefail

# Optional: copy host credentials into the container's throwaway HOME.
if [ -f /run/secrets/claude-credentials.json ]; then
  mkdir -p "$HOME/.claude"
  cp /run/secrets/claude-credentials.json "$HOME/.claude/.credentials.json"
  chmod 600 "$HOME/.claude/.credentials.json"
fi
if [ -f /run/secrets/pi-auth.json ]; then
  mkdir -p "$HOME/.pi/agent"
  cp /run/secrets/pi-auth.json "$HOME/.pi/agent/auth.json"
  chmod 600 "$HOME/.pi/agent/auth.json"
fi

case "${BERGER_HARNESS:-both}" in
  claude) berger init ;;
  pi) berger init --harness pi ;;
  both)
    berger init
    berger init --harness pi
    ;;
  *)
    echo "unknown BERGER_HARNESS: $BERGER_HARNESS (expected claude, pi, or both)" >&2
    exit 1
    ;;
esac

TMUX_CONF="$HOME/.config/berger/tmux.conf"
if ! grep -qsF "$TMUX_CONF" "$HOME/.tmux.conf" 2>/dev/null; then
  echo "source-file $TMUX_CONF" >> "$HOME/.tmux.conf"
fi

exec "$@"
