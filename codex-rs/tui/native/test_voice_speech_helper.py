#!/usr/bin/env python3
"""Native word-event smoke test; uses only the standard library and a disposable binary."""
import argparse
import base64
import select
import subprocess
import sys
import time


def run_case(binary: str, sentence: str, *, pause: bool = False, voice: str = "") -> list[str]:
    process = subprocess.Popen(
        [binary],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1,
    )
    assert process.stdin is not None and process.stdout is not None
    encoded = base64.b64encode(sentence.encode("utf-8")).decode("ascii")
    process.stdin.write(f"SPEAK\t{encoded}\t{voice}\n")
    process.stdin.flush()
    lines: list[str] = []
    deadline = time.monotonic() + 20
    paused = False
    try:
        while time.monotonic() < deadline:
            ready, _, _ = select.select([process.stdout], [], [], 0.5)
            if not ready:
                continue
            line = process.stdout.readline().strip()
            if not line:
                continue
            lines.append(line)
            if pause and line.startswith("WORD\t") and not paused:
                process.stdin.write("PAUSE\n")
                process.stdin.flush()
                time.sleep(0.2)
                process.stdin.write("RESUME\n")
                process.stdin.flush()
                paused = True
            if line.startswith("DONE") or line.startswith("ERROR"):
                break
        assert lines[-1] == "DONE\ttrue", f"speech failed: {lines!r}"
        words = [line for line in lines if line.startswith("WORD\t")]
        assert len(words) >= 3, f"missing native word events: {lines!r}"
        starts = [int(line.split("\t")[1]) for line in words]
        assert starts == sorted(starts), f"word positions not monotonic: {starts!r}"
        assert not pause or paused
        return lines
    finally:
        if process.poll() is None:
            process.stdin.write("STOP\n")
            process.stdin.flush()
        process.wait(timeout=4)


def test_barge_in(binary: str) -> None:
    process = subprocess.Popen(
        [binary], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, bufsize=1
    )
    assert process.stdin is not None and process.stdout is not None
    sentence = "Interrupt this long speech immediately. " * 12
    encoded = base64.b64encode(sentence.encode("utf-8")).decode("ascii")
    process.stdin.write(f"SPEAK\t{encoded}\t\n")
    process.stdin.flush()
    deadline = time.monotonic() + 8
    while time.monotonic() < deadline:
        ready, _, _ = select.select([process.stdout], [], [], 0.5)
        if ready and process.stdout.readline().startswith("WORD\t"):
            break
    else:
        process.kill()
        raise AssertionError("speech did not emit an initial word")
    process.stdin.write("STOP\n")
    process.stdin.flush()
    process.wait(timeout=3)
    assert process.poll() is not None


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("binary")
    parser.add_argument("--german-voice", default="", help="installed native voice identifier")
    args = parser.parse_args()
    de = run_case(args.binary, "Hallo, das ist ein deutscher Test. Noch eine zweite Zeile.", pause=True, voice=args.german_voice)
    if args.german_voice:
        assert not any(line.startswith("WARN") for line in de), "selected German voice not installed"
    en = run_case(args.binary, "Hello, this is an English test. Here is a second sentence.")
    print(f"German callbacks: {sum(x.startswith('WORD') for x in de)}, pause/resume: passed")
    print(f"English callbacks: {sum(x.startswith('WORD') for x in en)}, completed: passed")
    test_barge_in(args.binary)
    print("Immediate native stop after word callback: passed")
