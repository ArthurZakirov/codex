# Configuration

For basic configuration instructions, see [this documentation](https://developers.openai.com/codex/config-basic).

For advanced configuration instructions, see [this documentation](https://developers.openai.com/codex/config-advanced).

For a full configuration reference, see [this documentation](https://developers.openai.com/codex/config-reference).

## Lifecycle hooks

Admins can set top-level `allow_managed_hooks_only = true` in
`requirements.toml` to ignore user, project, and session hook configs while
still allowing managed hooks from requirements and managed config layers. This
setting is only supported in `requirements.toml`; putting it in `config.toml`
does not enable managed-hooks-only mode.

## Portable macOS Voice focus

Portable Codex Voice tabs share one audio slot per macOS user. Starting a call
while another updated tab owns the slot displays a message. Stop Voice in the
owning tab to release it. Replies stay visible when the slot is busy.

Set the focus policy in `~/.codex/voice-focus.json` (or in the session's Codex
home if configured). For example:

```json
{"mode":"stop_on_blur"}
```

Choose the policy for focus changes:

| Mode | Behavior |
| --- | --- |
| `manual` (default) | Start and stop Voice yourself. The audio slot still prevents overlap. |
| `stop_on_blur` | Stop Voice and local narration when the terminal loses focus. Start Voice manually when you return. |
| `follow_focus` | Stop on focus loss and start Voice when a terminal with an existing conversation gains focus and the slot is available. |

Codex reads this file on focus changes. The automatic policies require terminal
focus reports. For Warp, enable
[focus reporting](https://docs.warp.dev/terminal/settings/all-settings/) and use
Codex's alternate screen. Focus can also change when switching windows or
applications.
