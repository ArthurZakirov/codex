//! Opt-in local TTS for all visible backend assistant messages on macOS.
//! The realtime model still handles listening and delegation, but never rewrites speech.

use super::*;
use pulldown_cmark::Event;
use pulldown_cmark::Parser;
use pulldown_cmark::Tag;
use pulldown_cmark::TagEnd;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;

const MAX_NARRATED_ITEM_IDS: usize = 256;

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

/// One speech synthesizer at a time: subsequent commentary and final answers
/// must not stop a message that is already being spoken.
async fn speak_spoken_text(spoken: String) -> std::io::Result<()> {
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
    input.write_all(spoken.as_bytes()).await?;
    drop(input);
    let status = child.wait().await?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "macOS speech synthesizer exited with {status}"
        )));
    }
    Ok(())
}

/// A single consumer ensures FIFO playback and prevents later messages from
/// interrupting an earlier one even when the agent emits them rapidly.
async fn drain_speech_queue<F, Fut>(
    mut receiver: tokio::sync::mpsc::UnboundedReceiver<String>,
    mut synthesize: F,
) where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = std::io::Result<()>>,
{
    while let Some(spoken) = receiver.recv().await {
        if let Err(error) = synthesize(spoken).await {
            tracing::warn!("macOS speech synthesis failed: {error}");
        }
    }
}

impl ChatWidget {
    pub(crate) fn realtime_verbatim_speech_enabled(&self) -> bool {
        enabled()
    }

    pub(crate) fn cancel_verbatim_speech(&mut self) {
        self.realtime_conversation.verbatim_speech_sender.take();
        if let Some(task) = self.realtime_conversation.verbatim_speech_task.take() {
            task.abort();
        }
    }

    /// Include every completed assistant message that is shown in the live TUI:
    /// typed replies, progress/commentary, and voice-delegated final answers.
    /// Do not read out transcript snapshots or items already narrated.
    pub(crate) fn narrate_visible_agent_message(
        &mut self,
        turn_id: &str,
        item_id: &str,
        markdown: &str,
        from_replay: bool,
    ) {
        if !should_narrate_visible_agent_message(
            enabled(),
            self.realtime_conversation.phase == RealtimeConversationPhase::Active,
            from_replay,
            markdown,
        ) || self
            .realtime_conversation
            .narrated_agent_items
            .iter()
            .any(|(turn, item)| turn == turn_id && item == item_id)
        {
            return;
        }
        match self.play_verbatim_speech(markdown) {
            Ok(()) => {
                let items = &mut self.realtime_conversation.narrated_agent_items;
                if items.len() == MAX_NARRATED_ITEM_IDS {
                    items.pop_front();
                }
                items.push_back((turn_id.to_string(), item_id.to_string()));
            }
            Err(error) => self.add_error_message(format!(
                "Unable to read this assistant message aloud: {error}"
            )),
        }
    }

    /// Queue local playback rather than cancelling the preceding assistant
    /// message. The worker exits when voice mode is explicitly stopped.
    pub(crate) fn play_verbatim_speech(&mut self, markdown: &str) -> std::io::Result<()> {
        let spoken = markdown_to_spoken_text(markdown);
        if spoken.is_empty() {
            return Ok(());
        }
        if let Some(sender) = &self.realtime_conversation.verbatim_speech_sender
            && sender.send(spoken.clone()).is_ok()
        {
            return Ok(());
        }
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel::<String>();
        sender
            .send(spoken)
            .map_err(|_| std::io::Error::other("Unable to queue macOS speech"))?;
        self.realtime_conversation.verbatim_speech_sender = Some(sender);
        self.realtime_conversation.verbatim_speech_task = Some(tokio::spawn(drain_speech_queue(
            receiver,
            speak_spoken_text,
        )));
        Ok(())
    }
}

fn should_narrate_visible_agent_message(
    native_speech_enabled: bool,
    voice_active: bool,
    from_replay: bool,
    visible_text: &str,
) -> bool {
    native_speech_enabled && voice_active && !from_replay && !visible_text.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::drain_speech_queue;
    use super::markdown_to_spoken_text;
    use super::should_narrate_visible_agent_message;

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

    #[test]
    fn narrate_completed_visible_messages_without_origin_or_phase_filter() {
        for visible_message in [
            "Progress update marked commentary.",
            "Final answer to voice request.",
            "Final answer to typed request.",
            "Earlier completed answer after a newer input.",
        ] {
            assert!(should_narrate_visible_agent_message(
                true,
                true,
                false,
                visible_message
            ));
        }
    }

    #[tokio::test]
    async fn spoken_messages_are_queued_in_order_and_never_replace_earlier_messages() {
        use std::sync::Arc;
        use std::sync::Mutex;

        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let spoken = Arc::new(Mutex::new(Vec::new()));
        let first_is_done = Arc::new(tokio::sync::Notify::new());
        let started = Arc::new(tokio::sync::Notify::new());

        let handle = tokio::spawn(drain_speech_queue(receiver, {
            let spoken = Arc::clone(&spoken);
            let first_is_done = Arc::clone(&first_is_done);
            let started = Arc::clone(&started);
            move |message| {
                let spoken = Arc::clone(&spoken);
                let first_is_done = Arc::clone(&first_is_done);
                let started = Arc::clone(&started);
                async move {
                    if message == "Commentary" {
                        started.notify_one();
                        first_is_done.notified().await;
                    }
                    spoken.lock().unwrap().push(message);
                    Ok(())
                }
            }
        }));

        sender.send("Commentary".to_string()).unwrap();
        started.notified().await;
        sender.send("Typed reply".to_string()).unwrap();
        sender.send("Final answer".to_string()).unwrap();
        assert!(spoken.lock().unwrap().is_empty());
        first_is_done.notify_one();
        drop(sender);
        handle.await.unwrap();
        assert_eq!(
            *spoken.lock().unwrap(),
            ["Commentary", "Typed reply", "Final answer"]
        );
    }

    #[test]
    fn suppress_replay_empty_and_inactive_voice_messages() {
        assert!(!should_narrate_visible_agent_message(
            false, true, false, "Answer"
        ));
        assert!(!should_narrate_visible_agent_message(
            true, false, false, "Answer"
        ));
        assert!(!should_narrate_visible_agent_message(
            true,
            true,
            true,
            "Replayed answer"
        ));
        assert!(!should_narrate_visible_agent_message(
            true, true, false, " \n "
        ));
    }
}
