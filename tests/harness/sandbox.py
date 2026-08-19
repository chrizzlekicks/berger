"""TmuxSandbox: an isolated, throwaway tmux server for smoke-testing bergr.

Safety invariants (see plan doc for the empirical validation behind these):

  * The harness's OWN tmux calls always go through REAL_TMUX, resolved via
    shutil.which() at import time -- BEFORE any shim directory exists on PATH.
    Ordering is what guarantees this can never resolve to a shim.
  * Every bergr invocation happens with a `tmux` shim script prepended to
    PATH. The shim execs REAL_TMUX (the resolved absolute path, not a bare
    `tmux`) with `-L <this sandbox's socket>` spliced in, so bergr -- which
    calls bare `tmux` and never reads $TMUX -- can only ever reach the
    sandbox server, never the live `default` one.
  * Teardown (`kill-server` on the sandbox socket only) always runs, even on
    a failing assertion, via try/finally.
"""

import contextlib
import itertools
import json
import os
import shutil
import stat
import subprocess
import tempfile
import time
import unittest

REAL_TMUX = shutil.which("tmux")
if REAL_TMUX is None:
    raise unittest.SkipTest("tmux not found on PATH")
REAL_TMUX = os.path.realpath(REAL_TMUX)  # collapse symlinks, e.g. /bin/tmux -> /usr/bin/tmux

BERGR_BIN = os.path.realpath(
    os.path.join(os.path.dirname(__file__), "..", "..", "target", "debug", "bergr")
)

_socket_counter = itertools.count()


def _real_tmux(*args, **kwargs):
    """Call the REAL tmux directly -- never through a shim. Used only for
    harness-owned setup/teardown, never on bergr's behalf."""
    return subprocess.run(
        [REAL_TMUX, *args], capture_output=True, text=True, timeout=10, **kwargs
    )


class TmuxSandbox(contextlib.AbstractContextManager):
    """One throwaway tmux server + isolated HOME/XDG_CACHE_HOME + PATH shim.

    Usage:
        with TmuxSandbox() as sb:
            sb.new_session("prj", "impl")
            out = sb.event('{"hook_event_name": "Stop"}', session="prj", window="impl")
            sb.wait_for_window("prj", "impl✓")
    """

    def __init__(self, *, dead=False):
        # dead=True: point the shim at a socket with NO server running, so
        # every tmux call bergr makes fails and it takes the documented
        # "not inside tmux" no-op path. Used for the outside-tmux test --
        # deliberately never a plain unshimmed subprocess, since we're
        # running inside a real, live tmux session ourselves.
        self._dead = dead
        n = next(_socket_counter)
        self.socket = f"bergr-test-{os.getpid()}-{n}"
        self._tmpdir = tempfile.mkdtemp(prefix="bergr-smoke-")
        self.home = os.path.join(self._tmpdir, "home")
        self.xdg_cache = os.path.join(self._tmpdir, "cache")
        self._shim_dir = os.path.join(self._tmpdir, "shim")
        os.makedirs(self.home, exist_ok=True)
        os.makedirs(self.xdg_cache, exist_ok=True)
        os.makedirs(self._shim_dir, exist_ok=True)
        self._write_shim()
        self._started = False

    def _write_shim(self):
        shim_path = os.path.join(self._shim_dir, "tmux")
        with open(shim_path, "w") as f:
            f.write(f"#!/bin/sh\nexec {REAL_TMUX} -L {self.socket} \"$@\"\n")
        os.chmod(shim_path, os.stat(shim_path).st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)

    def __enter__(self):
        # No bootstrap session: `run-shell -t <target>`'s -t only controls
        # where OUTPUT is displayed, not which session a bare `tmux
        # display-message -p '#S'` (no -t) resolves inside that shell -- that
        # resolves against the server's "current"/most-recent session. If we
        # started a throwaway bootstrap session first, bergr's own
        # current_session()/current_window_name() calls would silently
        # resolve to the wrong session (confirmed empirically). So the first
        # session ever created on this server IS the real one from
        # new_session(), making it "current" by construction.
        self._started = False
        return self

    def __exit__(self, *exc):
        try:
            if self._started:
                _real_tmux("-L", self.socket, "kill-server")
        except Exception:
            pass  # teardown must never mask the real test failure
        finally:
            shutil.rmtree(self._tmpdir, ignore_errors=True)
            # tmux doesn't always unlink its own socket file after
            # kill-server (and a `dead` sandbox never had a server there at
            # all) -- clean up the /tmp/tmux-<uid>/<socket> litter ourselves.
            sock_dir = os.environ.get("TMUX_TMPDIR", f"/tmp/tmux-{os.getuid()}")
            with contextlib.suppress(OSError):
                os.remove(os.path.join(sock_dir, self.socket))
        return False

    # -- session/window management (harness-owned, real tmux) --------------

    def new_session(self, session, window):
        args = ["-L", self.socket]
        if not self._started:
            args += ["-f", "/dev/null"]  # first call also starts the server, hermetically
        args += ["new-session", "-d", "-s", session, "-n", window]
        r = _real_tmux(*args)
        if r.returncode != 0:
            raise RuntimeError(f"new-session failed: {r.stderr}")
        self._started = True

    def new_window(self, session, window):
        # -d: do NOT switch the client to the new window. Without it, a bare
        # `tmux display-message -p '#W'` (no -t) inside a later run-shell
        # resolves to whichever window most recently became "current" --
        # confirmed empirically to silently redirect an event meant for one
        # window onto another. Same root cause, same fix, as the no-bootstrap-
        # session rule in __enter__.
        r = _real_tmux("-L", self.socket, "new-window", "-d", "-t", session, "-n", window)
        if r.returncode != 0:
            raise RuntimeError(f"new-window failed: {r.stderr}")

    def rename_window(self, session, window, new_name):
        _real_tmux("-L", self.socket, "rename-window", "-t", f"{session}:{window}", new_name)

    def windows(self, session):
        r = _real_tmux(
            "-L", self.socket, "list-windows", "-t", session,
            "-F", "#{window_index}:#{window_name}",
        )
        if r.returncode != 0:
            return None
        return [line.split(":", 1)[1] for line in r.stdout.splitlines() if line]

    # -- bergr invocations (always through the shim) ------------------------

    def _env_prefix(self, extra_env):
        """Build a `KEY=val ` prefix string for an in-shell command. The tmux
        server snapshots env at new-session time, so every run-shell command
        must set what it needs explicitly rather than relying on that snapshot."""
        env = {"HOME": self.home, "XDG_CACHE_HOME": self.xdg_cache, "PATH": f"{self._shim_dir}:{os.environ.get('PATH', '')}"}
        env.update(extra_env or {})
        return " ".join(f"{k}={_sh_quote(v)}" for k, v in env.items())

    def event(self, payload_json, *, session, window, agent=None, timeout=5.0):
        """Run `bergr event` inside a run-shell targeted at session:window, so
        `tmux display-message -p '#S'/'#W'` resolve to a real client context.
        run-shell's own stdout goes to tmux (not to us), so we redirect to a
        file and read it back; run-shell is async, so callers must poll for
        the effect (state file / window rename) rather than trusting return
        order.

        NOT safe to call concurrently for different windows without an
        explicit `agent=` override: this selects `window` as current before
        dispatch (see below), and that select is not atomic with run-shell's
        dispatch -- two overlapping calls can race and cross windows. Pass
        `agent=` (bypassing #W entirely) for concurrent-call tests, as
        SmokeConcurrency does."""
        # run-shell splits its command argument on newlines like a tmux config
        # file (confirmed empirically -- a literal heredoc's embedded newlines
        # never reach the child process). So the whole thing must be a single
        # line: wrap in `sh -c '...'` and feed the payload via printf with
        # explicit \n escapes rather than a real multi-line heredoc.
        # run-shell's `-t` only controls where OUTPUT is displayed, not which
        # window a bare `tmux display-message -p '#W'` (no -t) resolves
        # against inside the dispatched shell -- that always resolves to the
        # session's *active* window (confirmed empirically: firing an event
        # "at" a non-active window still updated the active one). So a
        # multi-window session must select the target window first.
        if not self._dead:
            _real_tmux("-L", self.socket, "select-window", "-t", f"{session}:{window}")

        out_path = os.path.join(self._tmpdir, f"out-{next(_socket_counter)}.txt")
        extra = {"BERGR_AGENT": agent} if agent is not None else {}
        prefix = self._env_prefix(extra)
        payload_for_printf = payload_json.replace("\\", "\\\\").replace('"', '\\"')
        inner = (
            f"printf \"%s\" \"{payload_for_printf}\" | "
            f"{prefix} {_sh_quote(BERGR_BIN)} event >{_sh_quote(out_path)} 2>&1"
        )
        cmd = f"sh -c {_sh_quote(inner)}"
        target = f"{session}:{window}" if not self._dead else None
        args = ["-L", self.socket, "run-shell"]
        if target:
            args += ["-t", target]
        args += [cmd]
        r = _real_tmux(*args)
        if r.returncode != 0:
            raise RuntimeError(f"run-shell dispatch failed: {r.stderr}")

        def _done():
            return os.path.exists(out_path)

        wait_for(_done, timeout=timeout)
        if os.path.exists(out_path):
            with open(out_path) as f:
                return f.read()
        return ""

    def run_bergr(self, *args, stdin=None, timeout=10):
        """Run a bergr subcommand as a plain (shimmed) subprocess -- used for
        sync/init/reset, which don't need a tmux client context the way
        `event` does (sync takes --session explicitly; init/reset don't talk
        to tmux windows at all)."""
        env = dict(os.environ)
        env["HOME"] = self.home
        env["XDG_CACHE_HOME"] = self.xdg_cache
        env["PATH"] = f"{self._shim_dir}:{env.get('PATH', '')}"
        return subprocess.run(
            [BERGR_BIN, *args],
            input=stdin, capture_output=True, text=True, timeout=timeout, env=env,
        )

    # -- state file inspection ----------------------------------------------

    def state_path(self, session, agent):
        return os.path.join(self.xdg_cache, "bergr", session, f"{agent}.state")

    def state(self, session, agent):
        path = self.state_path(session, agent)
        if not os.path.exists(path):
            return None
        result = {}
        with open(path) as f:
            for line in f:
                if "=" in line:
                    k, v = line.rstrip("\n").split("=", 1)
                    result[k] = v
        return result

    # -- polling --------------------------------------------------------------

    def wait_for(self, predicate, timeout=5.0, interval=0.025):
        return wait_for(predicate, timeout=timeout, interval=interval)

    def wait_for_window(self, session, name, timeout=5.0):
        return self.wait_for(lambda: name in (self.windows(session) or []), timeout=timeout)

    def wait_for_state(self, session, agent, timeout=5.0):
        return self.wait_for(lambda: self.state(session, agent) is not None, timeout=timeout)

    def wait_for_no_state(self, session, agent, timeout=5.0):
        return self.wait_for(lambda: self.state(session, agent) is None, timeout=timeout)


def wait_for(predicate, timeout=5.0, interval=0.025):
    """Poll `predicate` until truthy or timeout. Never a fixed sleep -- tmux
    run-shell dispatch is async, so tests must wait for the actual effect."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(interval)
    return predicate()


def _sh_quote(s):
    return "'" + str(s).replace("'", "'\\''") + "'"
