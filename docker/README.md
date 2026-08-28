# Manual test rig

Build and run a container with tmux, Claude Code, and bergr wired together, to
reproduce/verify behavior by hand.

```bash
docker build -f docker/Dockerfile -t bergr-test .
docker run -it --rm --name bergr-test bergr-test
```

This drops you into a tmux session named `test` with bergr's hooks and tmux
config already installed (`bergr init` ran in the entrypoint, against a
container-local `$HOME` — your real `~/.claude` is untouched).

## Watching renames from a second terminal

```bash
docker exec -it bergr-test tmux attach
```

## Driving events without live Claude auth

Inside the container, rename a window to an agent name, then fire synthetic
hook events at it:

```bash
tmux rename-window impl
fake-event PermissionRequest   # window suffix -> !
fake-event Stop                # window suffix -> ✓
fake-event SessionEnd          # suffix cleared
```

## Testing with a real Claude Code session

The whole point of this container is testing bergr *without* touching your
real `~/.claude` on the host — so credentials go in read-only, and bergr's
own hook config stays entirely inside the container.

**Option A — API key** (simplest, no login flow):

```bash
docker run -it --rm --name bergr-test \
  -e ANTHROPIC_API_KEY \
  bergr-test
```

**Option B — reuse your existing Claude Code login**, without letting the
container write back to your real settings: mount only the credentials file,
read-only, to the path the entrypoint expects:

```bash
docker run -it --rm --name bergr-test \
  -v ~/.claude/.credentials.json:/run/secrets/claude-credentials.json:ro \
  bergr-test
```

The entrypoint copies that into the container's own `~/.claude/`, then runs
`bergr init` on top — so hook wiring happens in the container's throwaway
`$HOME`, and your host `~/.claude/settings.json` is never opened, let alone
rewritten. Don't bind-mount `~/.claude` directly (writable or not) — that
would let `bergr init` edit your real hook config.

Then just run `claude` in a renamed tmux window and watch the suffix track
its lifecycle.
