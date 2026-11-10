# Manual test rig

Build and run a container with tmux, Claude Code, Pi, and berger wired together:

```bash
docker build -f docker/Dockerfile -t berger-test .
docker run -it --rm --name berger-test berger-test
```

The default initializes both harnesses in a container-local `$HOME`. Select one
when needed:

```bash
docker run -it --rm -e BERGER_HARNESS=claude berger-test
docker run -it --rm -e BERGER_HARNESS=pi berger-test
```

Your host configuration is untouched. To watch window renames from another terminal:

```bash
docker exec -it berger-test tmux attach
```

## Synthetic Claude events

Inside the container:

```bash
tmux rename-window impl
fake-event PermissionRequest   # window suffix -> !
fake-event Stop                # window suffix -> ✓
fake-event SessionEnd          # suffix cleared
```

## Live Claude Code

Pass an API key:

```bash
docker run -it --rm --name berger-test \
  -e BERGER_HARNESS=claude \
  -e ANTHROPIC_API_KEY \
  berger-test
```

Or copy an existing login into the throwaway container home:

```bash
docker run -it --rm --name berger-test \
  -e BERGER_HARNESS=claude \
  -v ~/.claude/.credentials.json:/run/secrets/claude-credentials.json:ro \
  berger-test
```

Then rename the tmux window and run `claude`.

## Live Pi

The simplest option is a provider API key. For Anthropic:

```bash
docker run -it --rm --name berger-test \
  -e BERGER_HARNESS=pi \
  -e ANTHROPIC_API_KEY \
  berger-test
```

Alternatively copy Pi's saved credentials into the throwaway container home:

```bash
docker run -it --rm --name berger-test \
  -e BERGER_HARNESS=pi \
  -v ~/.pi/agent/auth.json:/run/secrets/pi-auth.json:ro \
  berger-test
```

Inside the container, verify authentication and choose an available model:

```bash
pi auth check --provider anthropic
pi --list-models
tmux rename-window impl
pi --provider anthropic --model <model-id>
```

During a run the window is `impl`; settlement changes it to `impl✓` or `impl✗`.
Quitting Pi clears the suffix.

Do not mount all of `~/.claude` or `~/.pi/agent`. Those directories contain
configuration, sessions, extensions, and credentials that the container should
not modify. Live provider tests make real requests and may incur cost.
