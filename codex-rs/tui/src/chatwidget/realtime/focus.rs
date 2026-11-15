//! Optional focus behavior for the portable Voice launcher.
use serde::Deserialize;
use serde::Serialize;
use std::path::Path;

#[derive(Clone, Copy, Default, Deserialize, PartialEq, Serialize, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Mode {
    #[default]
    Manual,
    StopOnBlur,
    FollowFocus,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    mode: Mode,
}

pub(super) fn read(config: Option<&Path>, codex_home: &Path) -> Mode {
    let value = config
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
    if let Some(mode) =
        value.and_then(|mut settings| settings.get_mut("focus_mode").map(std::mem::take))
    {
        return serde_json::from_value(mode).unwrap_or_default();
    }
    legacy_mode(codex_home)
}

pub(super) fn legacy_mode(codex_home: &Path) -> Mode {
    std::fs::read(codex_home.join("voice-focus.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Settings>(&bytes).ok())
        .map_or(Mode::Manual, |settings| settings.mode)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_explicit_follow_focus_enables_automatic_start() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("voice-focus.json");
        assert!(read(None, home.path()) == Mode::Manual);
        for (json, mode) in [
            (r#"{"mode":"stop_on_blur"}"#, Mode::StopOnBlur),
            (r#"{"mode":"follow_focus"}"#, Mode::FollowFocus),
            (r#"{"mode":"follow_focus","typo":true}"#, Mode::Manual),
            (r#"{"mode":"unknown"}"#, Mode::Manual),
        ] {
            std::fs::write(&path, json).unwrap();
            assert!(read(None, home.path()) == mode);
        }
    }
}
