//! Bounded, asynchronous command-backed status-line providers.
//! Programs are configured by the user and run without a shell. Their output
//! is advisory only; failures are explicitly shown as unknown.

use codex_config::types::TuiStatusLineCommand;
use codex_protocol::ThreadId;
use std::path::PathBuf;
use std::time::Duration;
use tokio::process::Command;

pub(crate) const REFRESH_INTERVAL: Duration = Duration::from_secs(120);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(25);
const MAX_DISPLAY_CHARS: usize = 110;

pub(crate) async fn run_commands(
    commands: Vec<TuiStatusLineCommand>,
    thread_id: ThreadId,
    cwd: PathBuf,
) -> Vec<String> {
    let mut displays = Vec::with_capacity(commands.len());
    for spec in commands {
        let mut command = Command::new(&spec.command);
        command
            .args(&spec.args)
            .current_dir(&cwd)
            .env("CODEX_THREAD_ID", thread_id.to_string())
            .kill_on_drop(true);
        let output = tokio::time::timeout(COMMAND_TIMEOUT, command.output()).await;
        let text = match output {
            Ok(Ok(output)) if output.status.success() => String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .filter(|c| !c.is_control())
                .take(MAX_DISPLAY_CHARS)
                .collect::<String>(),
            Ok(Err(error)) => {
                tracing::warn!(label = %spec.label, %error, "custom status provider failed");
                String::new()
            }
            Err(_) => {
                tracing::warn!(label = %spec.label, "custom status provider timed out");
                String::new()
            }
            _ => String::new(),
        };
        displays.push(if text.trim().is_empty() {
            format!("{} ?", spec.label)
        } else {
            text.trim().to_owned()
        });
    }
    displays
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[tokio::test]
    async fn command_receives_thread_id_and_outputs_visible_status() {
        let thread_id = ThreadId::from_string("01234567-89ab-cdef-0123-456789abcdef").unwrap();
        let commands = vec![TuiStatusLineCommand {
            label: "AGENTS".into(),
            command: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                r#"printf 'AGENTS %s\n' "$CODEX_THREAD_ID""#.into(),
            ],
        }];

        let result = run_commands(commands, thread_id, std::env::temp_dir()).await;

        assert_eq!(result, vec![format!("AGENTS {thread_id}")]);
    }

    #[tokio::test]
    async fn missing_provider_shows_unknown_instead_of_false_green() {
        let thread_id = ThreadId::from_string("01234567-89ab-cdef-0123-456789abcdef").unwrap();
        let commands = vec![TuiStatusLineCommand {
            label: "AGENTS".into(),
            command: "/definitely/not/a/command".into(),
            args: vec![],
        }];

        let result = run_commands(commands, thread_id, std::env::temp_dir()).await;

        assert_eq!(result, vec!["AGENTS ?"]);
    }
}
