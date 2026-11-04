//! Opt-in local TTS for word-faithful backend answers on macOS.
//! The realtime model still handles listening and delegation, but never rewrites speech.

use super::*;
use pulldown_cmark::Event;
use pulldown_cmark::Parser;
use pulldown_cmark::Tag;
use pulldown_cmark::TagEnd;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;

pub(crate) fn enabled() -> bool {
    std::env::var("CODEX_REALTIME_VERBATIM_SPEECH").is_ok_and(|value| value == "1")
}

/// Convert Markdown presentation into the same ordered words for the speech engine.
/// The original Markdown remains unchanged in the chat history.
fn markdown_to_spoken_text(markdown: &str) -> String {
    let mut spoken = String::new();
    for event in Parser::new(markdown) {
        match event {
            Event::Text(text) | Event::Code(text) => spoken.push_str(&text),
            Event::SoftBreak | Event::HardBreak | Event::Rule => spoken.push('\n'),
            Event::Start(
                Tag::Paragraph
                | Tag::Heading { .. }
                | Tag::Item
                | Tag::CodeBlock(_)
                | Tag::TableRow,
            ) => {
                if !spoken.is_empty() && !spoken.ends_with('\n') {
                    spoken.push('\n');
                }
            }
            Event::End(
                TagEnd::Paragraph
                | TagEnd::Heading(_)
                | TagEnd::Item
                | TagEnd::CodeBlock
                | TagEnd::TableRow,
            ) => {
                if !spoken.ends_with('\n') {
                    spoken.push('\n');
                }
            }
            Event::End(TagEnd::TableCell) => spoken.push_str(". "),
            Event::TaskListMarker(checked) => {
                spoken.push_str(if checked { "checked. " } else { "unchecked. " });
            }
            Event::FootnoteReference(label) => spoken.push_str(&label),
            _ => {}
        }
    }
    spoken.trim().to_string()
}

/// Controls the macOS synthesizer without stopping the realtime microphone session.
pub(super) struct VerbatimSpeechPlayback {
    pid: libc::pid_t,
    paused: bool,
    task: tokio::task::JoinHandle<()>,
}

impl VerbatimSpeechPlayback {
    fn toggle_pause(&mut self) -> std::io::Result<Option<bool>> {
        if self.task.is_finished() {
            return Ok(None);
        }
        let paused = !self.paused;
        let signal = if paused { libc::SIGSTOP } else { libc::SIGCONT };
        // SAFETY: pid comes only from the process spawned by play_verbatim_speech.
        if unsafe { libc::kill(self.pid, signal) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                return Ok(None);
            }
            return Err(error);
        }
        self.paused = paused;
        Ok(Some(paused))
    }
}

impl Drop for VerbatimSpeechPlayback {
    fn drop(&mut self) {
        // The child's kill_on_drop also stops a synthesizer left SIGSTOP-ed.
        self.task.abort();
    }
}

impl ChatWidget {
    pub(crate) fn realtime_verbatim_speech_enabled(&self) -> bool {
        enabled()
    }

    pub(crate) fn cancel_verbatim_speech(&mut self) {
        self.realtime_conversation.verbatim_speech.take();
    }

    /// Returns None when no live local speech process can be paused or resumed.
    pub(crate) fn toggle_verbatim_speech_pause(&mut self) -> std::io::Result<Option<bool>> {
        let Some(playback) = self.realtime_conversation.verbatim_speech.as_mut() else {
            return Ok(None);
        };
        let result = playback.toggle_pause()?;
        if result.is_none() {
            self.cancel_verbatim_speech();
        }
        Ok(result)
    }

    pub(crate) fn play_verbatim_speech(&mut self, markdown: &str) -> std::io::Result<()> {
        let spoken = markdown_to_spoken_text(markdown);
        self.cancel_verbatim_speech();
        if spoken.is_empty() {
            return Ok(());
        }
        // Send speech only over stdin, never process arguments or application logs.
        let mut child = tokio::process::Command::new("/usr/bin/say")
            .arg("-f")
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let mut input = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("macOS speech synthesizer has no input stream"))?;
        let pid = child
            .id()
            .and_then(|id| libc::pid_t::try_from(id).ok())
            .ok_or_else(|| std::io::Error::other("macOS speech synthesizer has no process ID"))?;
        let task = tokio::spawn(async move {
            if let Err(error) = input.write_all(spoken.as_bytes()).await {
                tracing::warn!("failed to deliver text to macOS speech synthesizer: {error}");
                return;
            }
            drop(input);
            match child.wait().await {
                Ok(status) if !status.success() => {
                    tracing::warn!("macOS speech synthesizer exited with {status}");
                }
                Err(error) => tracing::warn!("macOS speech synthesis failed: {error}"),
                _ => {}
            }
        });
        self.realtime_conversation.verbatim_speech = Some(VerbatimSpeechPlayback {
            pid,
            paused: false,
            task,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::VerbatimSpeechPlayback;
    use super::markdown_to_spoken_text;

    #[tokio::test]
    async fn pause_and_resume_keep_the_same_synthesizer_process() {
        let mut child = tokio::process::Command::new("/bin/sleep")
            .arg("30")
            .kill_on_drop(true)
            .spawn()
            .expect("start a harmless stand-in for macOS speech");
        let pid = child.id().expect("stand-in has pid") as libc::pid_t;
        let task = tokio::spawn(async move {
            let _ = child.wait().await;
        });
        let mut playback = VerbatimSpeechPlayback {
            pid,
            paused: false,
            task,
        };

        assert_eq!(playback.toggle_pause().unwrap(), Some(true));
        assert!(playback.paused);
        // Signal delivery is asynchronous: poll boundedly for the stopped state.
        let mut stopped = false;
        for _ in 0..20 {
            let status = std::process::Command::new("/bin/ps")
                .args(["-o", "state=", "-p", &pid.to_string()])
                .output()
                .unwrap();
            if String::from_utf8_lossy(&status.stdout)
                .trim()
                .starts_with('T')
            {
                stopped = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(stopped, "speech process should be suspended");

        assert_eq!(playback.toggle_pause().unwrap(), Some(false));
        assert!(!playback.paused);
        // Dropping the task kills the child, including a paused child.
        drop(playback);
    }

    #[tokio::test]
    async fn completed_speech_cannot_be_paused() {
        let task = tokio::spawn(async {});
        while !task.is_finished() {
            tokio::task::yield_now().await;
        }
        // An ended task must not send signals to any process.
        let mut playback = VerbatimSpeechPlayback {
            pid: i32::MAX,
            paused: false,
            task,
        };
        assert_eq!(playback.toggle_pause().unwrap(), None);
    }

    #[test]
    fn code_and_explanation_are_spoken_once_and_in_order() {
        let markdown = "**Squash** combines commits.\n\n```bash\ngit switch feature\ngit merge main\n```\n\n**Rebase** replays commits.";
        assert_eq!(
            markdown_to_spoken_text(markdown),
            "Squash combines commits.\ngit switch feature\ngit merge main\nRebase replays commits."
        );
    }

    #[test]
    fn pydantic_code_and_validation_rules_are_not_summarized() {
        let markdown = "Pydantic v2:\n\n```python\nclass User(BaseModel):\n    age: int = Field(ge=18)\n```\n\n- **Required:** age must be present.\n- **Constraint:** age >= 18.";
        let speech = markdown_to_spoken_text(markdown);
        assert_eq!(
            speech,
            "Pydantic v2:\nclass User(BaseModel):\n    age: int = Field(ge=18)\nRequired: age must be present.\nConstraint: age >= 18."
        );
    }
}
