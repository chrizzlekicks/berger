"""Hook-JSON payload builders and the verified event -> (state, symbol) table.

Mirrors src/state.rs::state_for_event and State::symbol exactly. Keep these two
in sync with the Rust source if the contract changes -- that's the whole point
of a smoke test.
"""

import json

# hook_event_name -> (state_name, window_symbol)
# state_name is None for events that clear state (SessionEnd, unknown).
EVENT_TABLE = {
    "SessionStart": ("working", ""),
    "UserPromptSubmit": ("working", ""),
    "PreToolUse": ("working", ""),
    "PermissionRequest": ("approval", "!"),
    "PostToolUseFailure": ("error", "✗"),
    "StopFailure": ("error", "✗"),
    "Stop": ("done", "✓"),
    "SessionEnd": (None, None),
}

ALL_EVENTS = list(EVENT_TABLE.keys())


def hook_payload(event_name, **extra):
    """Build the minimal JSON payload bergr's HookPayload deserializer expects."""
    payload = {"hook_event_name": event_name}
    payload.update(extra)
    return json.dumps(payload)


def suffixed(agent, event_name):
    """Expected window name for `agent` after `event_name` fires."""
    state_name, symbol = EVENT_TABLE[event_name]
    if state_name is None:
        return agent
    return f"{agent}{symbol}"
