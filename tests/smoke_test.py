#!/usr/bin/env python3
"""bergr smoke-test suite -- runs bergr against a real, throwaway tmux server.

Reproduces (and automates) the manual smoke-test flow from legacy/README.md.
Every test runs against an isolated `tmux -L bergr-test-<pid>-<n>` server, an
isolated HOME/XDG_CACHE_HOME, and a `tmux` PATH shim that redirects bergr's
own tmux calls into that sandbox. The live tmux server this suite itself runs
inside of is never touched -- see tests/harness/sandbox.py for exactly how
and why.

Usage:
    cargo build && python3 tests/smoke_test.py
    python3 tests/smoke_test.py -v
    python3 tests/smoke_test.py SmokeLifecycle.test_permission_request_adds_bang
    BERGR_TEST_REAL_CLAUDE=1 python3 tests/smoke_test.py   # + real-claude case
"""

import json
import os
import shutil
import subprocess
import sys
import unittest

sys.path.insert(0, os.path.dirname(__file__))

from harness.sandbox import TmuxSandbox, BERGR_BIN
from harness.payloads import hook_payload, suffixed, EVENT_TABLE, ALL_EVENTS

FIXTURE_SETTINGS = os.path.join(os.path.dirname(__file__), "fixtures", "settings.json")


# ---------------------------------------------------------------------------
# A. Lifecycle
# ---------------------------------------------------------------------------

class SmokeLifecycle(unittest.TestCase):
    def test_every_event_sets_correct_window_and_state(self):
        for event_name, (state_name, _symbol) in EVENT_TABLE.items():
            with self.subTest(event=event_name):
                with TmuxSandbox() as sb:
                    sb.new_session("prj", "impl")
                    sb.event(hook_payload(event_name), session="prj", window="impl")
                    expected_window = suffixed("impl", event_name)
                    self.assertTrue(
                        sb.wait_for_window("prj", expected_window),
                        f"window not renamed to {expected_window!r}, got {sb.windows('prj')}",
                    )
                    if state_name is None:
                        self.assertTrue(sb.wait_for_no_state("prj", "impl"))
                    else:
                        self.assertTrue(sb.wait_for_state("prj", "impl"))
                        self.assertEqual(sb.state("prj", "impl")["state"], state_name)

    def test_full_lifecycle_sequence(self):
        with TmuxSandbox() as sb:
            sb.new_session("prj", "impl")
            sequence = [
                ("SessionStart", "impl"),
                ("UserPromptSubmit", "impl"),
                ("PreToolUse", "impl"),
                ("PermissionRequest", "impl!"),
                ("Stop", "impl✓"),
            ]
            for event_name, expected in sequence:
                sb.event(hook_payload(event_name), session="prj", window="impl")
                self.assertTrue(
                    sb.wait_for_window("prj", expected),
                    f"after {event_name}: expected {expected!r}, got {sb.windows('prj')}",
                )

    def test_session_end_clears_state_and_suffix(self):
        with TmuxSandbox() as sb:
            sb.new_session("prj", "impl")
            sb.event(hook_payload("Stop"), session="prj", window="impl")
            self.assertTrue(sb.wait_for_window("prj", "impl✓"))
            sb.event(hook_payload("SessionEnd"), session="prj", window="impl✓")
            self.assertTrue(sb.wait_for_window("prj", "impl"))
            self.assertTrue(sb.wait_for_no_state("prj", "impl"))

    def test_repeated_event_is_idempotent(self):
        with TmuxSandbox() as sb:
            sb.new_session("prj", "impl")
            sb.event(hook_payload("PermissionRequest"), session="prj", window="impl")
            sb.wait_for_window("prj", "impl!")
            sb.event(hook_payload("PermissionRequest"), session="prj", window="impl!")
            sb.wait_for_state("prj", "impl")
            # give a mangled duplicate a chance to appear, then assert it didn't
            import time
            time.sleep(0.3)
            self.assertEqual(sb.windows("prj"), ["impl!"])

    def test_matches_window_already_bearing_a_suffix(self):
        with TmuxSandbox() as sb:
            sb.new_session("prj", "impl")
            sb.rename_window("prj", "impl", "impl!")
            sb.event(hook_payload("Stop"), session="prj", window="impl!")
            self.assertTrue(sb.wait_for_window("prj", "impl✓"))


# ---------------------------------------------------------------------------
# B. Agent resolution
# ---------------------------------------------------------------------------

class SmokeAgentResolution(unittest.TestCase):
    def test_bergr_agent_env_wins_over_window_name(self):
        with TmuxSandbox() as sb:
            sb.new_session("prj", "somewindow")
            sb.event(hook_payload("Stop"), session="prj", window="somewindow", agent="impl")
            self.assertTrue(sb.wait_for_state("prj", "impl"))
            self.assertIsNone(sb.state("prj", "somewindow"))

    def test_bergr_agent_env_suffix_is_stripped(self):
        with TmuxSandbox() as sb:
            sb.new_session("prj", "somewindow")
            sb.event(hook_payload("Stop"), session="prj", window="somewindow", agent="impl!")
            self.assertTrue(sb.wait_for_state("prj", "impl"))

    def test_empty_bergr_agent_falls_back_to_window_name(self):
        with TmuxSandbox() as sb:
            sb.new_session("prj", "impl")
            sb.event(hook_payload("Stop"), session="prj", window="impl", agent="")
            self.assertTrue(sb.wait_for_state("prj", "impl"))

    def test_no_bergr_agent_uses_window_name(self):
        with TmuxSandbox() as sb:
            sb.new_session("prj", "impl")
            sb.event(hook_payload("Stop"), session="prj", window="impl")
            self.assertTrue(sb.wait_for_state("prj", "impl"))


# ---------------------------------------------------------------------------
# C. Multi-window / multi-agent isolation
# ---------------------------------------------------------------------------

class SmokeIsolation(unittest.TestCase):
    def test_only_targeted_window_is_renamed(self):
        with TmuxSandbox() as sb:
            sb.new_session("prj", "impl")
            sb.new_window("prj", "plan")
            sb.event(hook_payload("Stop"), session="prj", window="impl")
            self.assertTrue(sb.wait_for_window("prj", "impl✓"))
            self.assertIn("plan", sb.windows("prj"))

    def test_two_agents_coexist_in_state_dir(self):
        with TmuxSandbox() as sb:
            sb.new_session("prj", "impl")
            sb.new_window("prj", "plan")
            sb.event(hook_payload("Stop"), session="prj", window="impl")
            sb.event(hook_payload("PermissionRequest"), session="prj", window="plan")
            self.assertTrue(sb.wait_for_state("prj", "impl"))
            self.assertTrue(sb.wait_for_state("prj", "plan"))
            self.assertEqual(sb.state("prj", "impl")["state"], "done")
            self.assertEqual(sb.state("prj", "plan")["state"], "approval")

    def test_event_for_agent_with_no_matching_window_still_writes_state(self):
        with TmuxSandbox() as sb:
            sb.new_session("prj", "impl")
            sb.event(hook_payload("Stop"), session="prj", window="impl", agent="ghost")
            self.assertTrue(sb.wait_for_state("prj", "ghost"))
            # the live window is renamed by BERGR_AGENT's name, not the window's own
            # prior name -- "impl" becomes "ghost✓", not "impl✓".
            self.assertEqual(sb.windows("prj"), ["ghost✓"])


# ---------------------------------------------------------------------------
# D. sync
# ---------------------------------------------------------------------------

class SmokeSync(unittest.TestCase):
    def test_sync_repairs_mangled_window_names(self):
        with TmuxSandbox() as sb:
            sb.new_session("prj", "impl")
            sb.event(hook_payload("Stop"), session="prj", window="impl")
            sb.wait_for_window("prj", "impl✓")
            sb.rename_window("prj", "impl✓", "impl")  # mangle back to bare name
            r = sb.run_bergr("sync", "--session", "prj")
            self.assertEqual(r.returncode, 0)
            self.assertEqual(sb.windows("prj"), ["impl✓"])

    def test_sync_on_session_with_no_state_dir_is_a_noop(self):
        with TmuxSandbox() as sb:
            sb.new_session("prj", "impl")
            r = sb.run_bergr("sync", "--session", "prj")
            self.assertEqual(r.returncode, 0)
            self.assertEqual(sb.windows("prj"), ["impl"])

    def test_sync_missing_session_flag_exits_1(self):
        with TmuxSandbox() as sb:
            r = sb.run_bergr("sync")
            self.assertEqual(r.returncode, 1)
            self.assertIn("--session", r.stderr)

    def test_sync_nonexistent_session_exits_1(self):
        with TmuxSandbox() as sb:
            sb.new_session("prj", "impl")  # starts the server, but not "ghost-session"
            r = sb.run_bergr("sync", "--session", "ghost-session")
            self.assertEqual(r.returncode, 1)

    def test_sync_is_idempotent(self):
        with TmuxSandbox() as sb:
            sb.new_session("prj", "impl")
            sb.event(hook_payload("Stop"), session="prj", window="impl")
            sb.wait_for_window("prj", "impl✓")
            sb.run_bergr("sync", "--session", "prj")
            before = sb.windows("prj")
            sb.run_bergr("sync", "--session", "prj")
            self.assertEqual(sb.windows("prj"), before)


# ---------------------------------------------------------------------------
# E. Robustness / negative paths -- event must never exit non-zero
# ---------------------------------------------------------------------------

class SmokeRobustness(unittest.TestCase):
    def test_malformed_json_is_a_clean_noop(self):
        with TmuxSandbox() as sb:
            sb.new_session("prj", "impl")
            out = sb.event("{not json", session="prj", window="impl")
            self.assertIn("malformed", out.lower())
            self.assertIsNone(sb.state("prj", "impl"))

    def test_empty_stdin_is_a_clean_noop(self):
        with TmuxSandbox() as sb:
            sb.new_session("prj", "impl")
            out = sb.event("", session="prj", window="impl")
            self.assertIn("malformed", out.lower())

    def test_missing_hook_event_name_is_a_clean_noop(self):
        with TmuxSandbox() as sb:
            sb.new_session("prj", "impl")
            out = sb.event(json.dumps({"some_other_field": 1}), session="prj", window="impl")
            self.assertIn("malformed", out.lower())
            self.assertIsNone(sb.state("prj", "impl"))

    def test_unknown_hook_event_name_clears_like_session_end(self):
        with TmuxSandbox() as sb:
            sb.new_session("prj", "impl")
            sb.event(hook_payload("Stop"), session="prj", window="impl")
            sb.wait_for_window("prj", "impl✓")
            sb.event(hook_payload("SomeFutureEvent"), session="prj", window="impl✓")
            self.assertTrue(sb.wait_for_window("prj", "impl"))
            self.assertTrue(sb.wait_for_no_state("prj", "impl"))

    def test_outside_tmux_is_a_documented_noop(self):
        # Deliberately NOT a plain unshimmed subprocess -- we're running
        # inside a real, live tmux session (TMUX is set), so a bare `tmux`
        # call would hit the actual default server. Point the shim at a dead
        # socket instead: every tmux call then fails -> None -> no-op.
        with TmuxSandbox(dead=True) as sb:
            r = sb.run_bergr("event", stdin=hook_payload("Stop"))
            self.assertEqual(r.returncode, 0)
            self.assertEqual(r.stdout, "")

    def test_agent_name_with_shell_metacharacters_is_safe(self):
        with TmuxSandbox() as sb:
            sb.new_session("prj", "impl")
            weird = "a; rm -rf /tmp/nope && echo $(whoami) `id`"
            sb.event(hook_payload("Stop"), session="prj", window="impl", agent=weird)
            self.assertTrue(sb.wait_for_state("prj", weird))
            # The window is renamed to this literal string (BERGR_AGENT + symbol),
            # never shell-interpreted -- had `$(whoami)`/backticks been expanded by a
            # shell along the way, this exact string would not survive as a window
            # name. That round-trip is what proves no shell execution occurred.
            self.assertEqual(sb.windows("prj"), [f"{weird}✓"])

    def test_hook_payload_with_shell_metacharacters_is_safe(self):
        with TmuxSandbox() as sb:
            sb.new_session("prj", "impl")
            # A field value chosen by whatever the hook payload's source
            # controls (e.g. a prompt or tool-input string), not by bergr or
            # the test harness. If this were shell-interpolated on the way to
            # `bergr event`'s stdin, `$(...)` would execute and `touch` would
            # run before bergr ever saw the JSON.
            marker = os.path.join(sb._tmpdir, "should-not-exist")
            payload = hook_payload("Stop", prompt=f"$(touch {marker}) `id` $HOME")
            sb.event(payload, session="prj", window="impl", agent="impl")
            self.assertTrue(sb.wait_for_state("prj", "impl"))
            self.assertFalse(os.path.exists(marker))

    def test_agent_name_with_unicode_is_safe(self):
        with TmuxSandbox() as sb:
            sb.new_session("prj", "impl")
            sb.event(hook_payload("Stop"), session="prj", window="impl", agent="日本語エージェント")
            self.assertTrue(sb.wait_for_state("prj", "日本語エージェント"))


# ---------------------------------------------------------------------------
# F. init (fake HOME)
# ---------------------------------------------------------------------------

def _copy_bergr_outside_target(dest_dir):
    """init refuses to run from a path containing 'target/'. Copy the built
    binary to a tmpdir so init's happy path can be exercised."""
    dest = os.path.join(dest_dir, "bergr")
    shutil.copy2(BERGR_BIN, dest)
    os.chmod(dest, 0o755)
    return dest


class SmokeInit(unittest.TestCase):
    def _run_init(self, sb, bergr_bin):
        env = dict(os.environ)
        env["HOME"] = sb.home
        env["XDG_CACHE_HOME"] = sb.xdg_cache
        env.pop("XDG_CONFIG_HOME", None)
        return subprocess.run([bergr_bin, "init"], capture_output=True, text=True, env=env, timeout=10)

    def test_migrates_amux_hooks_and_keeps_unrelated_ones(self):
        with TmuxSandbox() as sb:
            claude_dir = os.path.join(sb.home, ".claude")
            os.makedirs(claude_dir, exist_ok=True)
            shutil.copy2(FIXTURE_SETTINGS, os.path.join(claude_dir, "settings.json"))
            bergr_bin = _copy_bergr_outside_target(sb._tmpdir)

            r = self._run_init(sb, bergr_bin)
            self.assertEqual(r.returncode, 0, r.stderr)

            with open(os.path.join(claude_dir, "settings.json")) as f:
                settings = json.load(f)

            for event in ALL_EVENTS:
                entries = settings["hooks"].get(event, [])
                commands = [
                    h.get("command", "")
                    for e in entries
                    for h in e.get("hooks", [])
                ]
                self.assertFalse(
                    any("amux" in c for c in commands),
                    f"amux entry survived in {event}: {commands}",
                )
                bergr_entries = [c for c in commands if c.endswith(" event") and "bergr" in c]
                self.assertEqual(len(bergr_entries), 1, f"{event}: {commands}")

            pre_tool_use_cmds = [
                h.get("command", "")
                for e in settings["hooks"]["PreToolUse"]
                for h in e.get("hooks", [])
            ]
            self.assertTrue(any("rtk hook claude" in c for c in pre_tool_use_cmds))
            self.assertEqual(settings.get("model"), "opus")

    def test_creates_backup_matching_original(self):
        with TmuxSandbox() as sb:
            claude_dir = os.path.join(sb.home, ".claude")
            os.makedirs(claude_dir, exist_ok=True)
            settings_path = os.path.join(claude_dir, "settings.json")
            shutil.copy2(FIXTURE_SETTINGS, settings_path)
            with open(FIXTURE_SETTINGS, "rb") as f:
                original_bytes = f.read()
            bergr_bin = _copy_bergr_outside_target(sb._tmpdir)

            self._run_init(sb, bergr_bin)

            backup_path = settings_path[: -len(".json")] + ".json.bergr-bak"
            self.assertTrue(os.path.exists(backup_path))
            with open(backup_path, "rb") as f:
                self.assertEqual(f.read(), original_bytes)

    def test_rerunning_init_is_idempotent_and_backup_not_overwritten(self):
        with TmuxSandbox() as sb:
            claude_dir = os.path.join(sb.home, ".claude")
            os.makedirs(claude_dir, exist_ok=True)
            settings_path = os.path.join(claude_dir, "settings.json")
            shutil.copy2(FIXTURE_SETTINGS, settings_path)
            bergr_bin = _copy_bergr_outside_target(sb._tmpdir)

            self._run_init(sb, bergr_bin)
            backup_path = settings_path[: -len(".json")] + ".json.bergr-bak"
            with open(backup_path) as f:
                backup_after_first = f.read()

            with open(settings_path) as f:
                after_first = json.load(f)
            r2 = self._run_init(sb, bergr_bin)
            self.assertEqual(r2.returncode, 0, r2.stderr)
            with open(settings_path) as f:
                after_second = json.load(f)
            self.assertEqual(after_first, after_second)

            with open(backup_path) as f:
                self.assertEqual(f.read(), backup_after_first)

    def test_tmux_conf_contents(self):
        with TmuxSandbox() as sb:
            os.makedirs(os.path.join(sb.home, ".claude"), exist_ok=True)
            bergr_bin = _copy_bergr_outside_target(sb._tmpdir)
            r = self._run_init(sb, bergr_bin)
            self.assertEqual(r.returncode, 0, r.stderr)
            conf_path = os.path.join(sb.home, ".config", "bergr", "tmux.conf")
            with open(conf_path) as f:
                conf = f.read()
            self.assertIn("allow-rename off", conf)
            self.assertIn("automatic-rename off", conf)
            self.assertIn("bind-key M", conf)
            self.assertIn(f"'{bergr_bin}' sync --session", conf)

    def test_init_without_preexisting_settings_still_succeeds(self):
        with TmuxSandbox() as sb:
            os.makedirs(os.path.join(sb.home, ".claude"), exist_ok=True)
            bergr_bin = _copy_bergr_outside_target(sb._tmpdir)
            r = self._run_init(sb, bergr_bin)
            self.assertEqual(r.returncode, 0, r.stderr)
            settings_path = os.path.join(sb.home, ".claude", "settings.json")
            self.assertTrue(os.path.exists(settings_path))

    def test_init_creates_claude_dir_when_entirely_absent(self):
        # Regression test: init.rs now create_dir_all's ~/.claude before
        # writing settings.json (it already did this for ~/.config/bergr).
        # On a machine that has never run Claude Code -- no ~/.claude at all
        # -- `bergr init` must create it rather than failing with ENOENT.
        with TmuxSandbox() as sb:
            bergr_bin = _copy_bergr_outside_target(sb._tmpdir)
            r = self._run_init(sb, bergr_bin)
            self.assertEqual(r.returncode, 0, r.stderr)
            settings_path = os.path.join(sb.home, ".claude", "settings.json")
            self.assertTrue(os.path.exists(settings_path))

    def test_refuses_to_run_from_target_dir(self):
        with TmuxSandbox() as sb:
            # BERGR_BIN itself lives under target/debug -- exactly the guarded case.
            r = self._run_init(sb, BERGR_BIN)
            self.assertEqual(r.returncode, 1)
            self.assertIn("target", r.stderr.lower())

    def test_malformed_settings_json_exits_cleanly(self):
        with TmuxSandbox() as sb:
            claude_dir = os.path.join(sb.home, ".claude")
            os.makedirs(claude_dir, exist_ok=True)
            with open(os.path.join(claude_dir, "settings.json"), "w") as f:
                f.write("{not valid json")
            bergr_bin = _copy_bergr_outside_target(sb._tmpdir)
            r = self._run_init(sb, bergr_bin)
            self.assertEqual(r.returncode, 1)
            self.assertIn("not valid JSON", r.stderr)

    def test_empty_hooks_array_exits_cleanly_instead_of_panicking(self):
        # Regression test: merge_hooks() used to .expect() that "hooks" is an
        # object and panic on a hand-edited settings.json with "hooks": []
        # (an array). It now returns a Result, and init::run() reports the
        # shape mismatch as a normal exit-1 error instead of a panic.
        with TmuxSandbox() as sb:
            claude_dir = os.path.join(sb.home, ".claude")
            os.makedirs(claude_dir, exist_ok=True)
            with open(os.path.join(claude_dir, "settings.json"), "w") as f:
                json.dump({"hooks": []}, f)
            bergr_bin = _copy_bergr_outside_target(sb._tmpdir)
            r = self._run_init(sb, bergr_bin)
            self.assertEqual(r.returncode, 1)
            self.assertNotIn("panicked", r.stderr.lower())
            self.assertIn("unexpected shape", r.stderr.lower())


# ---------------------------------------------------------------------------
# G. reset
# ---------------------------------------------------------------------------

class SmokeReset(unittest.TestCase):
    def test_reset_removes_cache_root(self):
        with TmuxSandbox() as sb:
            sb.new_session("prj", "impl")
            sb.event(hook_payload("Stop"), session="prj", window="impl")
            sb.wait_for_state("prj", "impl")
            self.assertTrue(os.path.exists(os.path.join(sb.xdg_cache, "bergr")))
            r = sb.run_bergr("reset")
            self.assertEqual(r.returncode, 0)
            self.assertFalse(os.path.exists(os.path.join(sb.xdg_cache, "bergr")))

    def test_reset_with_nothing_to_remove_still_exits_0(self):
        with TmuxSandbox() as sb:
            r = sb.run_bergr("reset")
            self.assertEqual(r.returncode, 0)


# ---------------------------------------------------------------------------
# H. Opt-in real `claude` end-to-end
# ---------------------------------------------------------------------------

@unittest.skipUnless(os.environ.get("BERGR_TEST_REAL_CLAUDE") == "1", "set BERGR_TEST_REAL_CLAUDE=1 to run")
class SmokeRealClaude(unittest.TestCase):
    def test_real_claude_session_triggers_hooks_via_settings_json(self):
        claude_bin = shutil.which("claude")
        if not claude_bin:
            self.skipTest("claude not found on PATH")

        with TmuxSandbox() as sb:
            bergr_bin = _copy_bergr_outside_target(sb._tmpdir)
            init_env = dict(os.environ)
            init_env["HOME"] = sb.home
            init_env["XDG_CACHE_HOME"] = sb.xdg_cache
            init_env.pop("XDG_CONFIG_HOME", None)
            r = subprocess.run([bergr_bin, "init"], capture_output=True, text=True, env=init_env, timeout=10)
            self.assertEqual(r.returncode, 0, r.stderr)

            sb.new_session("prj", "impl")
            prefix = sb._env_prefix({})
            from harness.sandbox import _sh_quote, _real_tmux
            inner = f"{prefix} {_sh_quote(claude_bin)} -p 'say hi and stop' > /dev/null 2>&1"
            cmd = f"sh -c {_sh_quote(inner)}"
            _real_tmux("-L", sb.socket, "run-shell", "-t", "prj:impl", cmd)

            changed = sb.wait_for(
                lambda: sb.windows("prj") != ["impl"], timeout=90.0
            )
            self.assertTrue(changed, f"window never changed from real claude run: {sb.windows('prj')}")


# ---------------------------------------------------------------------------
# I. Concurrency
# ---------------------------------------------------------------------------

class SmokeConcurrency(unittest.TestCase):
    def test_concurrent_events_for_different_agents_dont_corrupt_state(self):
        import threading

        with TmuxSandbox() as sb:
            sb.new_session("prj", "impl")
            sb.new_window("prj", "plan")

            errors = []

            def fire(window, agent):
                try:
                    sb.event(hook_payload("Stop"), session="prj", window=window, agent=agent)
                except Exception as e:
                    errors.append(e)

            threads = [
                threading.Thread(target=fire, args=("impl", "impl")),
                threading.Thread(target=fire, args=("plan", "plan")),
            ]
            for t in threads:
                t.start()
            for t in threads:
                t.join()

            self.assertEqual(errors, [])
            self.assertTrue(sb.wait_for_state("prj", "impl"))
            self.assertTrue(sb.wait_for_state("prj", "plan"))
            self.assertEqual(sb.state("prj", "impl")["agent"], "impl")
            self.assertEqual(sb.state("prj", "plan")["agent"], "plan")


# ---------------------------------------------------------------------------
# J. CLI surface (main.rs)
# ---------------------------------------------------------------------------

class SmokeCli(unittest.TestCase):
    def test_bare_command_exits_1_with_usage(self):
        with TmuxSandbox() as sb:
            r = sb.run_bergr()
            self.assertEqual(r.returncode, 1)
            self.assertIn("usage: bergr", r.stderr)

    def test_unknown_subcommand_exits_1(self):
        with TmuxSandbox() as sb:
            r = sb.run_bergr("frobnicate")
            self.assertEqual(r.returncode, 1)
            self.assertIn("unknown command 'frobnicate'", r.stderr)
            self.assertIn("usage: bergr", r.stderr)

    def test_sync_unknown_flag_exits_1(self):
        with TmuxSandbox() as sb:
            r = sb.run_bergr("sync", "--session", "prj", "--force")
            self.assertEqual(r.returncode, 1)
            self.assertIn("unknown argument '--force'", r.stderr)

    def test_sync_session_flag_with_no_value_exits_1(self):
        with TmuxSandbox() as sb:
            r = sb.run_bergr("sync", "--session")
            self.assertEqual(r.returncode, 1)
            self.assertIn("--session <name> is required", r.stderr)


# ---------------------------------------------------------------------------
# K. Suffix transitions and repeated states
# ---------------------------------------------------------------------------

class SmokeSuffixTransitions(unittest.TestCase):
    def test_working_state_strips_existing_suffix(self):
        with TmuxSandbox() as sb:
            sb.new_session("prj", "impl")
            sb.event(hook_payload("Stop"), session="prj", window="impl")
            self.assertTrue(sb.wait_for_window("prj", "impl✓"))
            sb.event(hook_payload("PreToolUse"), session="prj", window="impl✓")
            self.assertTrue(sb.wait_for_window("prj", "impl"))

    def test_repeated_error_events_do_not_stack_suffix(self):
        with TmuxSandbox() as sb:
            sb.new_session("prj", "impl")
            sb.event(hook_payload("PostToolUseFailure"), session="prj", window="impl")
            self.assertTrue(sb.wait_for_window("prj", "impl✗"))
            sb.event(hook_payload("StopFailure"), session="prj", window="impl✗")
            self.assertTrue(sb.wait_for_window("prj", "impl✗"))
            self.assertEqual(sb.windows("prj"), ["impl✗"])

    def test_agent_name_of_only_suffix_chars_renames_to_empty(self):
        with TmuxSandbox() as sb:
            sb.new_session("prj", "somewindow")
            sb.event(hook_payload("Stop"), session="prj", window="somewindow", agent="!!!")
            self.assertTrue(sb.wait_for_state("prj", ""))
            self.assertTrue(sb.wait_for_window("prj", "✓"))


# ---------------------------------------------------------------------------
# L. Agent switching on a single window (state-file identity)
# ---------------------------------------------------------------------------

class SmokeAgentSwitch(unittest.TestCase):
    def test_switching_agent_on_same_window_removes_old_state_file(self):
        with TmuxSandbox() as sb:
            sb.new_session("prj", "somewindow")
            sb.event(hook_payload("Stop"), session="prj", window="somewindow", agent="alpha")
            self.assertTrue(sb.wait_for_state("prj", "alpha"))
            sb.event(hook_payload("Stop"), session="prj", window="somewindow", agent="beta")
            self.assertTrue(sb.wait_for_state("prj", "beta"))
            self.assertTrue(sb.wait_for_no_state("prj", "alpha"))

    def test_agent_name_with_slash_is_encoded_to_single_path_component(self):
        with TmuxSandbox() as sb:
            sb.new_session("prj", "impl")
            sb.event(hook_payload("Stop"), session="prj", window="impl", agent="feature/foo")
            self.assertTrue(sb.wait_for_state("prj", "feature/foo"))
            state_dir = os.path.join(sb.xdg_cache, "bergr", "prj")
            self.assertEqual(os.listdir(state_dir), ["feature%2ffoo.state"])


# ---------------------------------------------------------------------------
# M. sync: multi-window repair and orphan pruning
# ---------------------------------------------------------------------------

class SmokeSyncRepair(unittest.TestCase):
    def test_sync_repairs_multiple_mangled_windows_at_once(self):
        with TmuxSandbox() as sb:
            sb.new_session("prj", "impl")
            sb.new_window("prj", "plan")
            sb.event(hook_payload("Stop"), session="prj", window="impl")
            sb.event(hook_payload("PermissionRequest"), session="prj", window="plan")
            sb.wait_for_window("prj", "impl✓")
            sb.wait_for_window("prj", "plan!")
            sb.rename_window("prj", "impl✓", "impl")
            sb.rename_window("prj", "plan!", "plan")
            r = sb.run_bergr("sync", "--session", "prj")
            self.assertEqual(r.returncode, 0)
            self.assertEqual(set(sb.windows("prj")), {"impl✓", "plan!"})

    def test_sync_prunes_state_for_a_closed_window(self):
        with TmuxSandbox() as sb:
            sb.new_session("prj", "impl")
            sb.new_window("prj", "plan")
            sb.event(hook_payload("Stop"), session="prj", window="plan", agent="plan")
            self.assertTrue(sb.wait_for_state("prj", "plan"))
            _real_tmux_kill_window(sb, "prj", "plan")
            r = sb.run_bergr("sync", "--session", "prj")
            self.assertEqual(r.returncode, 0)
            self.assertTrue(sb.wait_for_no_state("prj", "plan"))


def _real_tmux_kill_window(sb, session, window):
    from harness.sandbox import _real_tmux
    _real_tmux("-L", sb.socket, "kill-window", "-t", f"{session}:{window}")


# ---------------------------------------------------------------------------
# N. init: legacy amux ~/.tmux.conf source-line removal
# ---------------------------------------------------------------------------

class SmokeInitTmuxConfMigration(unittest.TestCase):
    def _run_init(self, sb, bergr_bin):
        env = dict(os.environ)
        env["HOME"] = sb.home
        env["XDG_CACHE_HOME"] = sb.xdg_cache
        env.pop("XDG_CONFIG_HOME", None)
        return subprocess.run([bergr_bin, "init"], capture_output=True, text=True, env=env, timeout=10)

    def test_removes_stale_amux_source_line_from_user_tmux_conf(self):
        with TmuxSandbox() as sb:
            os.makedirs(os.path.join(sb.home, ".claude"), exist_ok=True)
            amux_conf = os.path.join(sb.home, ".config", "amux", "tmux.conf")
            user_conf = os.path.join(sb.home, ".tmux.conf")
            with open(user_conf, "w") as f:
                f.write(f'set -g mouse on\nsource-file "{amux_conf}"\nset -g history-limit 5000\n')
            bergr_bin = _copy_bergr_outside_target(sb._tmpdir)

            r = self._run_init(sb, bergr_bin)
            self.assertEqual(r.returncode, 0, r.stderr)

            with open(user_conf) as f:
                contents = f.read()
            self.assertNotIn("amux", contents)
            self.assertIn("history-limit 5000", contents)

            backup_path = user_conf[: -len(".conf")] + ".conf.bergr-bak"
            self.assertTrue(os.path.exists(backup_path))

    def test_leaves_user_tmux_conf_untouched_when_not_sourcing_amux(self):
        with TmuxSandbox() as sb:
            os.makedirs(os.path.join(sb.home, ".claude"), exist_ok=True)
            user_conf = os.path.join(sb.home, ".tmux.conf")
            with open(user_conf, "w") as f:
                f.write("set -g mouse on\n")
            bergr_bin = _copy_bergr_outside_target(sb._tmpdir)

            r = self._run_init(sb, bergr_bin)
            self.assertEqual(r.returncode, 0, r.stderr)

            with open(user_conf) as f:
                self.assertEqual(f.read(), "set -g mouse on\n")
            backup_path = user_conf[: -len(".conf")] + ".conf.bergr-bak"
            self.assertFalse(os.path.exists(backup_path))


# ---------------------------------------------------------------------------
# O. reset: legacy amux cache removal
# ---------------------------------------------------------------------------

class SmokeResetLegacy(unittest.TestCase):
    def test_reset_removes_legacy_amux_cache_root(self):
        with TmuxSandbox() as sb:
            amux_cache = os.path.join(sb.xdg_cache, "amux")
            os.makedirs(os.path.join(amux_cache, "prj"), exist_ok=True)
            with open(os.path.join(amux_cache, "prj", "watch.pid"), "w") as f:
                f.write("12345")
            r = sb.run_bergr("reset")
            self.assertEqual(r.returncode, 0)
            self.assertFalse(os.path.exists(amux_cache))


if __name__ == "__main__":
    if not os.path.exists(BERGR_BIN):
        print(f"error: bergr binary not found at {BERGR_BIN} -- run `cargo build` first", file=sys.stderr)
        sys.exit(1)
    unittest.main()
