//! Client-owned explicit spoken turn completion for experimental macOS voice.
//! The model transcribes speech; it does not decide whether "go" was submitted.

use super::*;
use codex_app_server_protocol::AskForApproval;

const MAX_SPOKEN_REQUEST_BYTES: usize = 4096;

#[derive(Debug, Default)]
pub(super) struct VoiceTurnBuffer {
    segments: Vec<String>,
    bytes: usize,
}

#[derive(Debug, PartialEq, Eq)]
enum VoiceTurnAction {
    Pending,
    Submit(String),
    Empty,
    TooLong,
}

impl VoiceTurnBuffer {
    fn accept(&mut self, transcript: &str) -> VoiceTurnAction {
        let segment = transcript.trim();
        if segment.is_empty() {
            return VoiceTurnAction::Pending;
        }
        let command = segment.trim_matches(|ch: char| {
            ch.is_whitespace() || matches!(ch, '.' | ',' | '!' | '?' | ':' | ';')
        });
        if command.eq_ignore_ascii_case("go") {
            let text = self.segments.join(" ");
            self.clear();
            return if text.is_empty() {
                VoiceTurnAction::Empty
            } else {
                VoiceTurnAction::Submit(text)
            };
        }
        if self.bytes + segment.len() + usize::from(!self.segments.is_empty())
            > MAX_SPOKEN_REQUEST_BYTES
        {
            self.clear();
            return VoiceTurnAction::TooLong;
        }
        self.bytes += segment.len() + usize::from(!self.segments.is_empty());
        self.segments.push(segment.to_string());
        VoiceTurnAction::Pending
    }

    pub(super) fn clear(&mut self) {
        self.segments.clear();
        self.bytes = 0;
    }
}

fn delegated_input(text: &str) -> String {
    let escaped = text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!("<realtime_delegation>\n  <input>{escaped}</input>\n</realtime_delegation>")
}

impl ChatWidget {
    pub(super) fn on_client_managed_voice_transcript(&mut self, transcript: &str) {
        if self.realtime_conversation.phase != RealtimeConversationPhase::Active {
            return;
        }
        match self.realtime_conversation.voice_turn.accept(transcript) {
            VoiceTurnAction::Pending | VoiceTurnAction::Empty => {}
            VoiceTurnAction::TooLong => {
                self.add_realtime_error(
                    "Spoken request exceeded the voice buffer limit. Repeat the request and say go."
                        .to_string(),
                );
            }
            VoiceTurnAction::Submit(transcript) => {
                if self.thread_id() != self.realtime_conversation.thread_id {
                    self.add_realtime_error(
                        "Voice request could not be sent: the active thread changed.".to_string(),
                    );
                    return;
                }
                let mode = self.effective_collaboration_mode();
                if mode.model().is_empty() {
                    self.add_realtime_error(
                        "Voice request could not be sent: the backend model is unavailable."
                            .to_string(),
                    );
                    return;
                }
                let collaboration_mode = self
                    .collaboration_modes_enabled()
                    .then(|| {
                        self.active_collaboration_mask
                            .as_ref()
                            .map(|_| mode.clone())
                    })
                    .flatten();
                let items = vec![UserInput::Text {
                    text: delegated_input(&transcript),
                    text_elements: Vec::new(),
                }];
                let op = AppCommand::user_turn(
                    "user",
                    uuid::Uuid::new_v4().to_string(),
                    items,
                    self.config.cwd.to_path_buf(),
                    AskForApproval::from(self.config.permissions.approval_policy.value()),
                    self.config.permissions.active_permission_profile(),
                    mode.model().to_string(),
                    mode.reasoning_effort(),
                    None,
                    self.service_tier_update_for_core(),
                    None,
                    collaboration_mode,
                );
                if !self.submit_op(op) {
                    self.add_realtime_error("Failed to submit the spoken request.".to_string());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spoken_go_submits_all_prior_segments_once() {
        let mut input = VoiceTurnBuffer::default();
        assert_eq!(
            input.accept("Please explain the difference"),
            VoiceTurnAction::Pending
        );
        assert_eq!(
            input.accept("between a Pydantic model and a Python dataclass"),
            VoiceTurnAction::Pending
        );
        assert_eq!(
            input.accept("Go."),
            VoiceTurnAction::Submit(
                "Please explain the difference between a Pydantic model and a Python dataclass"
                    .into()
            )
        );
        assert_eq!(input.accept("go"), VoiceTurnAction::Empty);
    }

    #[test]
    fn go_inside_a_sentence_does_not_submit() {
        let mut input = VoiceTurnBuffer::default();
        assert_eq!(
            input.accept("I want to go through the code"),
            VoiceTurnAction::Pending
        );
        assert_eq!(
            input.accept("GO!"),
            VoiceTurnAction::Submit("I want to go through the code".into())
        );
    }

    #[test]
    fn request_is_escaped_before_being_sent_to_backend() {
        let wrapped = delegated_input("explain a < b & x > y");
        assert_eq!(
            realtime_delegation_input(&[UserInput::Text {
                text: wrapped,
                text_elements: Vec::new(),
            }]),
            Some("explain a &lt; b &amp; x &gt; y")
        );
    }

    #[tokio::test]
    async fn voice_go_submits_one_backend_turn_without_typing() {
        let (mut chat, _sender, _events, mut ops) =
            crate::chatwidget::tests::make_chatwidget_manual_with_sender().await;
        let thread_id = ThreadId::new();
        chat.thread_id = Some(thread_id);
        chat.realtime_conversation.thread_id = Some(thread_id);
        chat.realtime_conversation.phase = RealtimeConversationPhase::Active;
        chat.realtime_conversation.latest_input_was_voice = true;
        chat.on_client_managed_voice_transcript("Explain Pydantic models");
        chat.on_client_managed_voice_transcript("and Python dataclasses");
        assert!(ops.try_recv().is_err(), "do not submit until spoken go");
        chat.on_client_managed_voice_transcript("Go!");
        let AppCommand::UserTurn {
            items,
            turn_trigger,
            ..
        } = ops.try_recv().expect("spoken go must reach backend")
        else {
            panic!("expected a backend user turn");
        };
        assert_eq!(turn_trigger, "user");
        assert_eq!(
            realtime_delegation_input(&items),
            Some("Explain Pydantic models and Python dataclasses")
        );
        chat.on_client_managed_voice_transcript("go");
        assert!(
            ops.try_recv().is_err(),
            "repeated go must not duplicate a turn"
        );
    }

    #[test]
    fn bounded_buffer_rejects_oversized_requests() {
        let mut input = VoiceTurnBuffer::default();
        assert_eq!(
            input.accept(&"a".repeat(MAX_SPOKEN_REQUEST_BYTES + 1)),
            VoiceTurnAction::TooLong
        );
        assert_eq!(input.accept("go"), VoiceTurnAction::Empty);
    }
}
