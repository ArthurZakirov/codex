//! Opt-in local TTS for all visible backend assistant messages on macOS.
//! The realtime model still handles listening and delegation, but never rewrites speech.

use super::*;
use pulldown_cmark::Event;
use pulldown_cmark::Parser;
use pulldown_cmark::Tag;
use pulldown_cmark::TagEnd;
use std::process::ExitStatus;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

const MAX_NARRATED_ITEM_IDS: usize = 256;
// Apple's on-device NaturalLanguage recognizer requires no API key, download,
// or extra runtime. Send text by stdin rather than in process arguments.
const DETECT_SPEECH_LANGUAGE_JXA: &str = r#"
ObjC.import("Foundation");
ObjC.import("NaturalLanguage");
var data = $.NSFileHandle.fileHandleWithStandardInput.readDataToEndOfFile;
var text = $.NSString.alloc.initWithDataEncoding(data, $.NSUTF8StringEncoding);
var lang = $.NLLanguageRecognizer.dominantLanguageForString(text);
lang ? ObjC.unwrap(lang) : "unknown";
"#;

/// Choose only voices actually installed on this Mac. Prefer a familiar voice
/// in each language but accept other local voices with the same locale.
fn voice_for_language(language: &str, available: &str) -> Option<String> {
    let (locale, preferred) = match language.trim() {
        "de" => ("de_DE", "Anna"),
        "en" => ("en_US", "Samantha"),
        _ => return None,
    };
    let marker = format!(" {locale}");
    let mut fallback = None;
    for line in available.lines() {
        let Some(position) = line.find(&marker) else {
            continue;
        };
        let name = line[..position].trim();
        if name == preferred {
            return Some(name.to_string());
        }
        if !name.is_empty() && fallback.is_none() {
            fallback = Some(name.to_string());
        }
    }
    fallback
}

/// Detect the language of the actual assistant answer on the local Mac.
async fn detect_spoken_language(spoken: &str) -> Option<String> {
    let mut child = tokio::process::Command::new("/usr/bin/osascript")
        .args(["-l", "JavaScript", "-e", DETECT_SPEECH_LANGUAGE_JXA])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    let mut input = child.stdin.take()?;
    // Bound classification work even when a reply contains long code blocks.
    let sample: String = spoken.chars().take(4_000).collect();
    let write_result =
        tokio::time::timeout(Duration::from_secs(2), input.write_all(sample.as_bytes())).await;
    if !matches!(write_result, Ok(Ok(()))) {
        return None;
    }
    drop(input);
    let output = tokio::time::timeout(Duration::from_secs(3), child.wait_with_output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(std::str::from_utf8(&output.stdout).ok()?.trim().to_string())
}

/// Resolve a downloaded voice for this language, else retain the system voice.
/// The installed voice list may differ across Apple Silicon Macs.
async fn detect_speech_voice(spoken: &str) -> Option<String> {
    let language = detect_spoken_language(spoken).await?;
    let installed = tokio::time::timeout(
        Duration::from_secs(3),
        tokio::process::Command::new("/usr/bin/say")
            .args(["-v", "?"])
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !installed.status.success() {
        return None;
    }
    let voices = std::str::from_utf8(&installed.stdout).ok()?;
    let selected = voice_for_language(&language, voices);
    if selected.is_none() && matches!(language.as_str(), "de" | "en") {
        tracing::warn!("No installed macOS TTS voice for {language}; using the system default");
    }
    selected
}

/// A handle for the currently speaking synthesizer. A queued message does not
/// make Space a playback shortcut until a real speech process has started.
pub(super) struct SpeechPlaybackControl {
    paused: tokio::sync::watch::Sender<bool>,
    active: Arc<AtomicBool>,
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

/// One speech synthesizer at a time: subsequent commentary and final answers
/// must not stop a message that is already being spoken.
/// Only the worker holding this Child can signal its PID. This avoids the TUI
/// signaling a stale PID after the synthesizer exits and the PID is reused.
async fn wait_for_speech_process(
    mut child: tokio::process::Child,
    mut pause_rx: tokio::sync::watch::Receiver<bool>,
    active: Arc<AtomicBool>,
) -> std::io::Result<ExitStatus> {
    let pid = child
        .id()
        .and_then(|id| libc::pid_t::try_from(id).ok())
        .ok_or_else(|| std::io::Error::other("macOS speech synthesizer has no process ID"))?;
    active.store(true, Ordering::Release);
    let _active_guard = ActiveSpeechGuard(active);
    let mut suspended = false;
    loop {
        tokio::select! {
            status = child.wait() => return status,
            updated = pause_rx.changed() => {
                if updated.is_err() {
                    return child.wait().await;
                }
                let requested = *pause_rx.borrow_and_update();
                if requested == suspended {
                    continue;
                }
                if let Some(status) = child.try_wait()? {
                    return Ok(status);
                }
                // SAFETY: pid belongs to the Child held by this task; check
                // termination before signaling and never expose it to the UI.
                let signal = if requested { libc::SIGSTOP } else { libc::SIGCONT };
                if unsafe { libc::kill(pid, signal) } != 0 {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() == Some(libc::ESRCH) {
                        return child.wait().await;
                    }
                    return Err(error);
                }
                suspended = requested;
            }
        }
    }
}

async fn speak_spoken_text(
    spoken: String,
    pause_rx: tokio::sync::watch::Receiver<bool>,
    active: Arc<AtomicBool>,
) -> std::io::Result<()> {
    // Identify the visible answer language before playback starts, preserving
    // FIFO ordering and the process-based Space pause/resume control.
    let voice = detect_speech_voice(&spoken).await;
    let mut command = tokio::process::Command::new("/usr/bin/say");
    if let Some(voice) = voice {
        command.args(["-v", voice.as_str()]);
    }
    // Deliver text by stdin, never command arguments or application logs.
    let mut child = command
        .args(["-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut input = child
        .stdin
        .take()
        .ok_or_else(|| std::io::Error::other("macOS speech synthesizer has no input stream"))?;
    // Writing concurrently allows pause/resume even for long spoken messages
    // whose content would otherwise fill the synthesizer's stdin pipe.
    let writer = tokio::spawn(async move { input.write_all(spoken.as_bytes()).await });
    let status = wait_for_speech_process(child, pause_rx, active).await?;
    writer.await.map_err(std::io::Error::other)??;
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
        self.realtime_conversation.verbatim_speech_control.take();
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
        self.realtime_conversation.verbatim_speech_sender = Some(sender);
        self.realtime_conversation.verbatim_speech_control = Some(SpeechPlaybackControl {
            paused: pause_sender.clone(),
            active: Arc::clone(&active),
        });
        self.realtime_conversation.verbatim_speech_task =
            Some(tokio::spawn(drain_speech_queue(receiver, move |spoken| {
                // Each new spoken item starts unpaused. All queued items retain
                // their original FIFO order during an earlier item's pause.
                pause_sender.send_replace(false);
                speak_spoken_text(spoken, pause_receiver.clone(), Arc::clone(&active))
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
    use super::detect_spoken_language;
    use super::drain_speech_queue;
    use super::markdown_to_spoken_text;
    use super::should_narrate_visible_agent_message;
    use super::voice_for_language;
    use super::wait_for_speech_process;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering;

    #[test]
    fn language_selection_prefers_installed_voices_and_falls_back_safely() {
        let voices = "Eddy (English (US)) en_US  # Hello\n\
Anna                de_DE  # Hallo\n\
Samantha            en_US  # Hello\n\
Flo (German (Germany)) de_DE  # Hallo\n";
        assert_eq!(voice_for_language("de\n", voices), Some("Anna".to_string()));
        assert_eq!(
            voice_for_language("en\n", voices),
            Some("Samantha".to_string())
        );
        assert_eq!(
            voice_for_language("de", "Flo (German (Germany)) de_DE # Hallo"),
            Some("Flo (German (Germany))".to_string())
        );
        assert_eq!(voice_for_language("de", "Samantha en_US # Hello"), None);
        assert_eq!(voice_for_language("unknown", voices), None);
        assert_eq!(voice_for_language("fr", voices), None);
    }

    #[tokio::test]
    async fn local_macos_language_detection_uses_answer_text() {
        assert_eq!(
            detect_spoken_language(
                "Das ist eine deutsche Antwort. Ich erkläre dir, warum diese Funktion so arbeitet."
            )
            .await,
            Some("de".to_string())
        );
        assert_eq!(
            detect_spoken_language(
                "This is an English answer. I will explain how the implementation works."
            )
            .await,
            Some("en".to_string())
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
        let control = SpeechPlaybackControl {
            paused,
            active: Arc::clone(&active),
        };
        assert_eq!(control.toggle_pause(), None);
        active.store(true, Ordering::Release);
        assert_eq!(control.toggle_pause(), Some(true));
        assert_eq!(control.toggle_pause(), Some(false));
        active.store(false, Ordering::Release);
        assert_eq!(control.toggle_pause(), None);
    }

    #[tokio::test]
    async fn pause_and_resume_suspend_one_running_child_without_dropping_its_queue() {
        let child = tokio::process::Command::new("/bin/sleep")
            .arg("30")
            .kill_on_drop(true)
            .spawn()
            .expect("start a harmless stand-in for speech");
        let pid = child.id().expect("process PID");
        // Ownership transfers to the worker that is allowed to signal it.
        let (paused, receiver) = tokio::sync::watch::channel(false);
        let active = Arc::new(AtomicBool::new(false));
        let control = SpeechPlaybackControl {
            paused,
            active: Arc::clone(&active),
        };
        let worker = tokio::spawn(wait_for_speech_process(
            child,
            receiver,
            Arc::clone(&active),
        ));
        for _ in 0..50 {
            if control.is_active() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(control.is_active());
        assert_eq!(control.toggle_pause(), Some(true));

        let state = || {
            let output = std::process::Command::new("/bin/ps")
                .args(["-o", "state=", "-p", &pid.to_string()])
                .output()
                .expect("inspect child state");
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        };
        let mut stopped = false;
        for _ in 0..50 {
            if state().starts_with('T') {
                stopped = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(stopped, "speech process did not enter the stopped state");
        assert_eq!(control.toggle_pause(), Some(false));
        let mut resumed = false;
        for _ in 0..50 {
            let status = state();
            if !status.is_empty() && !status.starts_with('T') {
                resumed = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(resumed, "speech process did not resume");
        worker.abort();
        let _ = worker.await;
        assert!(!control.is_active());
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
