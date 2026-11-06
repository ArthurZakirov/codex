//! Opt-in local TTS for all visible backend assistant messages on macOS.
//! The realtime model still handles listening and delegation, but never rewrites speech.

use super::*;
use objc2_app_kit::NSSpeechBoundary;
#[allow(deprecated)]
use objc2_app_kit::NSSpeechSynthesizer;
use objc2_foundation::NSString;
use pulldown_cmark::Event;
use pulldown_cmark::Parser;
use pulldown_cmark::Tag;
use pulldown_cmark::TagEnd;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

const MAX_NARRATED_ITEM_IDS: usize = 256;

/// Controls the native speech synthesizer, including cancellation of a
/// paused utterance. Queued speech does not intercept Space until it begins.
pub(super) struct SpeechPlaybackControl {
    paused: tokio::sync::watch::Sender<bool>,
    active: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
}

impl SpeechPlaybackControl {
    fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    fn toggle_pause(&self) -> Option<bool> {
        if !self.is_active() {
            return None;
        }
        let next = !*self.paused.borrow();
        self.paused.send_replace(next);
        Some(next)
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

struct ActiveSpeechGuard(Arc<AtomicBool>);

impl Drop for ActiveSpeechGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpeechLanguage {
    German,
    English,
}

/// Technical identifiers should not overwhelm natural-language cues.
fn detect_speech_language(text: &str) -> SpeechLanguage {
    const GERMAN: &[&str] = &[
        "der", "die", "das", "und", "ist", "ich", "nicht", "mit", "für", "eine", "einen", "wir",
        "werden", "kann", "bitte", "diese", "wenn", "dann", "auch", "wird", "auf", "von", "zu",
        "den", "dem", "sie", "hallo", "guten", "morgen", "wie", "geht", "es", "dir", "mir", "du",
        "dein", "deine", "danke", "heute", "jetzt", "noch", "schon", "aber", "doch", "haben",
        "sind", "kein", "keine", "kannst", "können", "weiter", "mal",
    ];
    const ENGLISH: &[&str] = &[
        "the", "and", "is", "are", "with", "for", "this", "that", "you", "your", "will", "can",
        "please", "when", "then", "from", "not", "we", "have", "does", "into", "should", "it",
        "of", "to",
    ];
    let mut german = 0;
    let mut english = 0;
    for word in text
        .split(|c: char| !c.is_alphabetic())
        .filter(|w| !w.is_empty())
    {
        let lower = word.to_lowercase();
        if lower.chars().any(|c| matches!(c, 'ä' | 'ö' | 'ü' | 'ß')) {
            german += 3;
        }
        if GERMAN.contains(&lower.as_str()) {
            german += 1;
        }
        if ENGLISH.contains(&lower.as_str()) {
            english += 1;
        }
    }
    if german > english {
        SpeechLanguage::German
    } else {
        SpeechLanguage::English
    }
}

#[allow(deprecated)]
fn choose_speech_voice(language: SpeechLanguage) -> Option<String> {
    let voices = NSSpeechSynthesizer::availableVoices();
    let candidates: Vec<String> = voices.iter().map(|voice| voice.to_string()).collect();
    let locale = match language {
        SpeechLanguage::German => "de-de",
        SpeechLanguage::English => "en-us",
    };
    let preferred = match language {
        SpeechLanguage::German => "anna",
        SpeechLanguage::English => "samantha",
    };
    candidates
        .iter()
        .find(|voice| {
            let name = voice.to_ascii_lowercase();
            name.contains(locale) && name.ends_with(&format!(".{preferred}"))
        })
        .or_else(|| {
            candidates
                .iter()
                .find(|voice| voice.to_ascii_lowercase().contains(locale))
        })
        .cloned()
}

/// The speech engine, not its process, owns the playback position.
/// SIGSTOP on /usr/bin/say only suspends its client process; CoreAudio may
/// continue consuming queued audio, resulting in mute rather than true pause.
/// NSSpeechSynthesizer explicitly preserves its utterance position on pause.
#[allow(deprecated)]
fn speak_spoken_text_blocking(
    spoken: String,
    mut pause_rx: tokio::sync::watch::Receiver<bool>,
    active: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
) -> std::io::Result<()> {
    if cancelled.load(Ordering::Acquire) {
        return Ok(());
    }
    let synthesizer = NSSpeechSynthesizer::new();
    if let Some(voice) = choose_speech_voice(detect_speech_language(&spoken)) {
        if !synthesizer.setVoice(Some(&NSString::from_str(&voice))) {
            tracing::warn!("macOS speech voice {voice} was unavailable; using system default");
        }
    }
    if !synthesizer.startSpeakingString(&NSString::from_str(&spoken)) {
        return Err(std::io::Error::other(
            "macOS speech synthesizer could not start",
        ));
    }
    active.store(true, Ordering::Release);
    let _active_guard = ActiveSpeechGuard(active);
    let mut paused = false;
    while synthesizer.isSpeaking() {
        if cancelled.load(Ordering::Acquire) {
            synthesizer.stopSpeaking();
            return Ok(());
        }
        let requested_pause = *pause_rx.borrow_and_update();
        if requested_pause != paused {
            if requested_pause {
                synthesizer.pauseSpeakingAtBoundary(NSSpeechBoundary::ImmediateBoundary);
            } else {
                synthesizer.continueSpeaking();
            }
            paused = requested_pause;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

async fn speak_spoken_text(
    spoken: String,
    pause_rx: tokio::sync::watch::Receiver<bool>,
    active: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
) -> std::io::Result<()> {
    // Native AppKit synthesizers are managed on their owning thread.
    // Cancellation remains cooperative even when the queue task is aborted.
    tokio::task::spawn_blocking(move || {
        speak_spoken_text_blocking(spoken, pause_rx, active, cancelled)
    })
    .await
    .map_err(std::io::Error::other)?
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

    pub(crate) fn realtime_verbatim_speech_active(&self) -> bool {
        self.realtime_conversation
            .verbatim_speech_control
            .as_ref()
            .is_some_and(SpeechPlaybackControl::is_active)
    }

    /// Return None rather than intercept Space when no synthesizer is speaking.
    pub(crate) fn toggle_verbatim_speech_pause(&mut self) -> Option<bool> {
        self.realtime_conversation
            .verbatim_speech_control
            .as_ref()
            .and_then(SpeechPlaybackControl::toggle_pause)
    }

    pub(crate) fn cancel_verbatim_speech(&mut self) {
        self.realtime_conversation.verbatim_speech_sender.take();
        if let Some(control) = self.realtime_conversation.verbatim_speech_control.take() {
            control.cancel();
        }
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
        if !should_narrate_visible_agent_message(enabled(), from_replay, markdown)
            || self
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
        let (pause_sender, pause_receiver) = tokio::sync::watch::channel(false);
        let active = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::new(AtomicBool::new(false));
        self.realtime_conversation.verbatim_speech_sender = Some(sender);
        self.realtime_conversation.verbatim_speech_control = Some(SpeechPlaybackControl {
            paused: pause_sender.clone(),
            active: Arc::clone(&active),
            cancelled: Arc::clone(&cancelled),
        });
        self.realtime_conversation.verbatim_speech_task =
            Some(tokio::spawn(drain_speech_queue(receiver, move |spoken| {
                // Each new spoken item starts unpaused. All queued items retain
                // their original FIFO order during an earlier item's pause.
                pause_sender.send_replace(false);
                speak_spoken_text(
                    spoken,
                    pause_receiver.clone(),
                    Arc::clone(&active),
                    Arc::clone(&cancelled),
                )
            })));
        Ok(())
    }
}

fn should_narrate_visible_agent_message(
    native_speech_enabled: bool,
    from_replay: bool,
    visible_text: &str,
) -> bool {
    // The dedicated codex-voice launcher enables local narration. Typed
    // replies must be audible even before the microphone call is started.
    native_speech_enabled && !from_replay && !visible_text.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::SpeechPlaybackControl;
    use super::drain_speech_queue;
    use super::markdown_to_spoken_text;
    use super::should_narrate_visible_agent_message;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering;

    #[test]
    fn detects_german_prose_and_technical_terms() {
        assert_eq!(
            super::detect_speech_language(
                "Bitte erkläre, wie wir den Pydantic Validator mit Git rebase testen können."
            ),
            super::SpeechLanguage::German
        );
        assert_eq!(
            super::detect_speech_language("Die Funktion ist für diese API nicht verfügbar."),
            super::SpeechLanguage::German
        );
        assert_eq!(
            super::detect_speech_language("Guten Morgen, wie geht es dir heute?"),
            super::SpeechLanguage::German
        );
        assert_eq!(
            super::detect_speech_language("Hallo, kannst du mir helfen?"),
            super::SpeechLanguage::German
        );
    }

    #[test]
    fn detects_english_prose_and_technical_terms() {
        assert_eq!(
            super::detect_speech_language(
                "Please explain how we can test the Pydantic validator with Git rebase."
            ),
            super::SpeechLanguage::English
        );
        assert_eq!(
            super::detect_speech_language("The function is not available for this API."),
            super::SpeechLanguage::English
        );
    }

    #[test]
    fn defaults_to_english_for_code_only_or_ambiguous_text() {
        assert_eq!(
            super::detect_speech_language("git switch feature; cargo test"),
            super::SpeechLanguage::English
        );
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
    fn space_toggles_only_when_a_speech_process_is_active() {
        let (paused, _receiver) = tokio::sync::watch::channel(false);
        let active = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::new(AtomicBool::new(false));
        let control = SpeechPlaybackControl {
            paused,
            active: Arc::clone(&active),
            cancelled,
        };
        assert_eq!(control.toggle_pause(), None);
        active.store(true, Ordering::Release);
        assert_eq!(control.toggle_pause(), Some(true));
        assert_eq!(control.toggle_pause(), Some(false));
        active.store(false, Ordering::Release);
        assert_eq!(control.toggle_pause(), None);
    }

    #[test]
    fn cancellation_reaches_a_paused_native_speech_worker() {
        let (paused, _receiver) = tokio::sync::watch::channel(false);
        let active = Arc::new(AtomicBool::new(true));
        let cancelled = Arc::new(AtomicBool::new(false));
        let control = SpeechPlaybackControl {
            paused,
            active,
            cancelled: Arc::clone(&cancelled),
        };
        assert_eq!(control.toggle_pause(), Some(true));
        control.cancel();
        assert!(cancelled.load(Ordering::Acquire));
        // The native speech worker notices cancellation even while paused.
        assert_eq!(control.toggle_pause(), Some(false));
    }

    #[test]
    fn dedicated_voice_launcher_reads_typed_answers_without_microphone_call() {
        assert!(should_narrate_visible_agent_message(
            true,
            false,
            "Typed answer without an active call"
        ));
        assert!(!should_narrate_visible_agent_message(
            false,
            false,
            "The normal Codex launcher must stay silent"
        ));
        assert!(!should_narrate_visible_agent_message(
            true,
            true,
            "Replayed answer"
        ));
        assert!(!should_narrate_visible_agent_message(true, false, " \n "));
    }
}
