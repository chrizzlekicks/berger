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
        env = {
            "HOME": self.home,
            "XDG_CACHE_HOME": self.xdg_cache,
            "PATH": f"{self._shim_dir}:{os.environ.get('PATH', '')}",
        }
        env.update(extra_env or {})
        assignments = " ".join(f"{k}={_sh_quote(v)}" for k, v in env.items())
        # A dev's own shell may export XDG_CONFIG_HOME; that must not leak into
        # bergr's config-dir resolution inside the sandbox.
        return f"XDG_CONFIG_HOME= {assignments}"

    def event(self, payload_json, *, session, window, agent=None, timeout=5.0):
        """Run `bergr event` inside a run-shell targeted at session:window, so
        `tmux display-message -p '#S'/'#W'` resolve to a real client context.
        run-shell's own stdout goes to tmux (not to us), so we redirect to a
        temp file and rename it into place only after `bergr event` exits --
        callers polling for `out_path` are then guaranteed to see a fully
        written file, never a partial one. The child's own exit code is
        appended as a trailing line and split off below, since run-shell is
        async and its own dispatch return code says nothing about whether
        `bergr event` itself exited zero.

        A bare `tmux display-message -p '#W'` inside the dispatched shell
        always resolves to the session's *active* window (confirmed
        empirically -- run-shell's `-t` only controls where output is
        displayed, not what `#W` resolves to, and bergr's own window
        rename/lookup logic likewise acts on the active window, not `-t`'s
        target). So the target window must be selected before dispatch. That
        selection and the dispatch are chained as a single `tmux cmd \;
        cmd` invocation -- one client request tmux executes as one atomic
        unit -- so concurrent calls for different windows can't interleave
        and cross windows the way two separate `select-window` /
        `run-shell` calls could."""
        # run-shell splits its command argument on newlines like a tmux config
        # file (confirmed empirically -- a literal heredoc's embedded newlines
        # never reach the child process). So the whole thing must be a single
        # line: wrap in `sh -c '...'` and feed the payload via printf with
        # explicit \n escapes rather than a real multi-line heredoc.
        out_path = os.path.join(self._tmpdir, f"out-{next(_socket_counter)}.txt")
        tmp_path = out_path + ".part"
        extra = {"BERGR_AGENT": agent} if agent is not None else {}
        prefix = self._env_prefix(extra)
        payload_for_printf = payload_json.replace("\\", "\\\\").replace('"', '\\"')
        inner = (
            f"printf \"%s\" \"{payload_for_printf}\" | "
            f"{prefix} {_sh_quote(BERGR_BIN)} event >{_sh_quote(tmp_path)} 2>&1; "
            f"printf \"\\n__EXIT__:%s\\n\" \"$?\" >>{_sh_quote(tmp_path)}; "
            f"mv {_sh_quote(tmp_path)} {_sh_quote(out_path)}"
        )
        cmd = f"sh -c {_sh_quote(inner)}"
        # `window` may already be stale by the time this is called (e.g.
        # renamed by a prior event on the same window under a different
        # agent) -- callers passing `agent=` rely on BERGR_AGENT for identity
        # and don't need `window` to still exist. Only target a window that's
        # actually still present, so a stale name degrades to "no target"
        # (output display falls back to the session's active window; harmless
        # since output is redirected to a file) instead of a hard failure.
        target = f"{session}:{window}" if not self._dead and window in (self.windows(session) or []) else None
        args = ["-L", self.socket]
        if target:
            args += ["select-window", "-t", target, ";"]
        args += ["run-shell"]
        if target:
            args += ["-t", target]
        args += [cmd]
        r = _real_tmux(*args)
        if r.returncode != 0:
            raise RuntimeError(f"run-shell dispatch failed: {r.stderr}")

        def _done():
            return os.path.exists(out_path)

        wait_for(_done, timeout=timeout)
        if not os.path.exists(out_path):
            return ""
        with open(out_path) as f:
            content = f.read()
        output, _, exit_line = content.rpartition("\n__EXIT__:")
        exit_code = int(exit_line.strip())
        if exit_code != 0:
            raise RuntimeError(f"bergr event exited {exit_code}: {output}")
        return output

    def run_bergr(self, *args, stdin=None, timeout=10):
        """Run a bergr subcommand as a plain (shimmed) subprocess -- used for
        sync/init/reset, which don't need a tmux client context the way
        `event` does (sync takes --session explicitly; init/reset don't talk
        to tmux windows at all)."""
        env = dict(os.environ)
        env["HOME"] = self.home
        env["XDG_CACHE_HOME"] = self.xdg_cache
        env.pop("XDG_CONFIG_HOME", None)
        env["PATH"] = f"{self._shim_dir}:{env.get('PATH', '')}"
        return subprocess.run(
            [BERGR_BIN, *args],
            input=stdin, capture_output=True, text=True, timeout=timeout, env=env,
        )

    # -- state file inspection ----------------------------------------------

    def state_path(self, session, agent):
        session_component = _encode_session_component(session)
        agent_component = _encode_path_component(agent)
        return os.path.join(self.xdg_cache, "bergr", session_component, f"{agent_component}.state")

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


def _escape_path_component(component):
    """Mirrors src/fs_util.rs::escape_path_component -- '/', '\\', '%' become
    %XX, byte-for-byte (bergr operates on raw bytes, not code points)."""
    encoded = []
    for b in component.encode("utf-8"):
        if b in (0x2F, 0x5C, 0x25):  # '/', '\\', '%'
            encoded.append(f"%{b:02x}")
        else:
            encoded.append(chr(b))
    return "".join(encoded)


def _encode_path_component(component):
    """Mirrors src/fs_util.rs::encode_path_component (agent names: escaped + lowercased)."""
    return _escape_path_component(component).lower()


def _encode_session_component(component):
    """Mirrors src/fs_util.rs::encode_session_component (session names: escaped, case kept)."""
    return _escape_path_component(component)
