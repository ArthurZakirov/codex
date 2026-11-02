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

impl ChatWidget {
    pub(crate) fn realtime_verbatim_speech_enabled(&self) -> bool {
        enabled()
    }

    pub(crate) fn cancel_verbatim_speech(&mut self) {
        if let Some(task) = self.realtime_conversation.verbatim_speech_task.take() {
            task.abort();
        }
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
        self.realtime_conversation.verbatim_speech_task = Some(tokio::spawn(async move {
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
        }));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::markdown_to_spoken_text;

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
