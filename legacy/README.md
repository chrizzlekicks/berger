# amux

tmux agent status helper for Claude Code.

Claude Code hooks write state files; `amux watch` reads them and renames tmux
windows to show each agent's current status as a suffix.

```
1:main  2:plan…  3:impl!  4:review✓  5:tests✗
```

## Symbols

| State    | Symbol | Meaning              |
|----------|--------|----------------------|
| working  | _(none)_ | Busy, no action needed |
| approval | !      | Needs your input     |
| done     | ✓      | Session finished     |
| error    | ✗      | Hook / tool error    |

## Quick start

### 1. Make sure amux is on your PATH

`amux` is installed at `~/.local/bin/amux`. Verify:

```bash
which amux   # should print ~/.local/bin/amux
amux --help
```

### 2. Add tmux settings

Add to `~/.tmux.conf`:

```tmux
set -g allow-rename off
set -g automatic-rename off

bind-key M run-shell "amux watch --session #{session_name} >/tmp/amux-#{session_name}.log 2>&1 &"
```

Or source the provided snippet:

```tmux
source-file ~/.config/amux/tmux.conf
```

### 3. Claude Code hooks

The hooks are already wired in `~/.claude/settings.json`. On `SessionStart`,
the watcher is started automatically and the state is set to `working`. No need
to press `prefix + M` manually.

| Event | State | Symbol |
|---|---|---|
| `SessionStart` | `working` | _(none)_ |
| `UserPromptSubmit` | `working` | _(none)_ |
| `PreToolUse` | `working` | _(none)_ |
| `PermissionRequest` | `approval` | `!` |
| `PostToolUseFailure` | `error` | `✗` |
| `Stop` | `done` | `✓` |
| `StopFailure` | `error` | `✗` |
| `SessionEnd` | _(state file deleted)_ | _(none)_ |

The agent name defaults to `$AMUX_AGENT` if set, otherwise `basename $PWD`.
Name your tmux windows to match your project directories (or set `AMUX_AGENT`
in the shell where you launch `claude`).

### 4. Start a session

```bash
# Start a new tmux session for your project
tmux new-session -d -s myproject -c ~/code/myproject

# Start the watcher in the background
amux watch --session myproject &

# Attach
tmux attach -t myproject
```

Or press `prefix + M` inside tmux to start the watcher for the current session.

### 5. Name your windows to match agent roles

```bash
# Inside tmux:
tmux rename-window impl
tmux rename-window plan
# etc.
```

When Claude Code runs inside one of these windows, the suffix updates
automatically.

## Manual usage

```bash
# Mark an agent state manually (useful for testing)
amux mark --agent impl --state working --session myproject
amux mark --agent impl --state approval --session myproject
amux mark --agent impl --state done --session myproject

# Show current state for all agents in a session
amux status --session myproject

# Start the watcher (idempotent — safe to run multiple times)
amux watch --session myproject
```

## Per-window agent name

Set `AMUX_AGENT` in the shell where you run `claude` to override the default
(basename of `$PWD`):

```bash
export AMUX_AGENT=impl
claude
```

## File locations

| What | Path |
|---|---|
| Binary | `~/.local/bin/amux` |
| tmux config | `~/.config/tmux/tmux.conf` (amux lines at the bottom) |
| Claude Code hooks | `~/.claude/settings.json` |
| State files | `~/.cache/amux/<session>/<agent>.state` |
| Watcher logs | `/tmp/amux-<session>.log` |

## Architecture

```
Claude Code hooks
  -> amux mark           (writes state file, never touches tmux)
  -> ~/.cache/amux/<session>/<agent>.state

amux watch               (reads state files every 2s, started by SessionStart hook)
  -> tmux rename-window  (appends symbol, strips old suffix first)
```

State files are plain key=value text — no JSON, no jq required.

## Troubleshooting

- **Window names not updating**: check that `allow-rename off` and
  `automatic-rename off` are set in tmux.
- **Multiple watchers**: `amux watch` is idempotent per session — a second
  invocation exits immediately if a watcher is already running.
- **Watcher log**: `cat /tmp/amux-<session>.log`
- **State files**: `ls ~/.cache/amux/<session>/`
