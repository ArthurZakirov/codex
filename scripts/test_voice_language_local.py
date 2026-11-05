#!/usr/bin/env python3
"""Fast, offline macOS TTS smoke test; no full Codex build or playback.

Exercises the exact JXA script embedded in the Rust implementation and confirms
both selected installed macOS voices can synthesize speech through stdin.
"""

from pathlib import Path
import platform
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
RUST_SOURCE = ROOT / "codex-rs/tui/src/chatwidget/realtime/verbatim_speech.rs"

CASES = (
    ("de", "Anna", "Das ist eine deutsche Erklärung darüber, warum diese Funktion so arbeitet."),
    ("en", "Samantha", "This is an English explanation about how this function works."),
)


def main() -> None:
    if platform.system() != "Darwin":
        raise SystemExit("This native voice smoke test requires macOS.")
    source = RUST_SOURCE.read_text()
    matched = re.search(
        r'const DETECT_SPEECH_LANGUAGE_JXA: &str = r#"(.*?)"#;', source, re.DOTALL
    )
    if matched is None:
        raise RuntimeError("Cannot find the production language-detection script.")

    with tempfile.TemporaryDirectory(prefix="codex-voice-local-") as temp:
        for expected, voice, text in CASES:
            detection = subprocess.run(
                ["/usr/bin/osascript", "-l", "JavaScript", "-e", matched.group(1)],
                input=text, text=True, capture_output=True, timeout=5, check=True,
            )
            actual = detection.stdout.strip()
            if actual != expected:
                raise AssertionError(f"Expected {expected}, detected {actual!r}.")

            audio = Path(temp) / f"{expected}.aiff"
            subprocess.run(
                ["/usr/bin/say", "-v", voice, "-f", "-", "-o", str(audio)],
                input=text, text=True, capture_output=True, timeout=30, check=True,
            )
            if audio.stat().st_size < 1_000:
                raise AssertionError(f"{voice} produced no usable audio.")
            print(f"PASS: {expected} detected; {voice} generated audio.")

    print("PASS: local German/English voice smoke test (no cloud or release).")


if __name__ == "__main__":
    main()
