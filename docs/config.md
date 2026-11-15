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

## Portable macOS Voice

Portable Codex Voice tabs share one audio slot per macOS user. Starting a call
while another updated tab owns the slot displays a message. Stop Voice in the
owning tab to release it. Replies stay visible when the slot is busy.

Run `/voice config` to open `~/.config/codex-voice/config.json` in VS Code.
The command fills in missing settings and imports the previous focus mode the
first time it opens a config without `focus_mode`.

The following example enables dictation for F8 and leaves automatic startup off.

```json
{
  "dictation": { "enabled": true, "auto_start": false },
  "focus_mode": "stop_on_blur",
  "speech_rate": 1.8
}
```

Enter submits dictated input. During a Voice Call, Enter with an empty
composer can send the recognized user caption through the normal chat input.
If recognition is still processing, Enter waits for the pending speech segment.
Press Enter again to submit the available text if recognition stalls.

With dictation disabled, `focus_mode` controls how the Voice Call responds to terminal focus.

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
