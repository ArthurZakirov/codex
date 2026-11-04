# Custom status-line providers (fork extension)

This fork extends the native `/statusline` picker with a `custom-commands`
item. Each configured command contributes one short string to the footer.
Codex executes commands asynchronously without a shell, once per 120 seconds
for the active thread. Each process has a 25-second timeout.

## Configuration

The custom fork accepts the following in `~/.codex/config.toml`:

```toml
[tui]
status_line = ["model-with-reasoning", "git-branch", "custom-commands", "context-used"]

[[tui.status_line_commands]]
label = "AGENTS"
command = "codex-agents-status"
```

**Do not put `tui.status_line_commands` in a config shared with unmodified
Codex**: older binaries do not recognize this fork-only key.

Only user, system, managed, or explicit runtime configuration may define executable
status providers. Project-local `.codex/config.toml` files cannot define
`tui.status_line_commands` or enable the `custom-commands` item. These
settings are removed before project configuration is merged, because status
commands execute on the host outside the agent sandbox.

Alternatively, a launcher can provide commands without changing the shared
configuration:

```bash
export CODEX_STATUS_LINE_COMMANDS='[{"label":"AGENTS","command":"codex-agents-status"}]'
exec /path/to/custom/codex "$@"
```

When no explicit `tui.status_line` list exists, a nonempty environment
configuration automatically adds `custom-commands` to the default status
line. An explicit `/statusline` selection remains authoritative.

## Provider contract

- `label`: shown with `?` if the program fails, times out, or prints nothing.
- `command`: executable path or PATH-resolved command; no shell evaluation.
- `args`: optional argument array.
- `CODEX_THREAD_ID`: exact UUID of the displayed conversation, set by Codex.
- **stdout**: first line is shown; control characters are removed and display
  text is bounded to 110 characters.
- Nonzero exit codes and invalid outputs never become a successful indicator.
- Results from old threads are discarded if the user changes conversations.

The AGENTS provider from the Dotfiles repository runs its existing five-stage
read-only checker. It reports `current`, `outdated`, or `unknown` separately
for GitHub, local main, generated guidance, chezmoi, and the current session.

An indicator that a skill's body is actually retained in the *next model
request* requires independent runtime evidence; installing a skill or merely
seeing its metadata is not sufficient. No skill-context claim is made by
this extension.
