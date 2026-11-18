// Word-position-aware macOS speech worker. All AppKit calls stay on the process main thread.
// stdin: SPEAK<TAB>base64(UTF-8)<TAB>voice-id<TAB>rate-multiplier, PAUSE, RESUME, STOP
// stdout: WORD<TAB>utf16-start<TAB>utf16-length, DONE, ERROR<TAB>reason
import AppKit
import Foundation

private func emit(_ message: String) {
    print(message)
    fflush(stdout)
}

private final class SpeechService: NSObject, NSSpeechSynthesizerDelegate {
    private var synthesizer = NSSpeechSynthesizer()
    private var source: NSString = ""
    private var starts = [0]
    private var base = 0
    private var current = 0
    private var lastBack: TimeInterval?
    private var muted = false

    private func seek(back: Bool) {
        guard started else { return }
        let index = starts.lastIndex(where: { $0 <= current }) ?? 0
        let now = ProcessInfo.processInfo.systemUptime
        let repeatBack = lastBack.map { now - $0 < 1.2 } ?? false
        let target = back ? (repeatBack ? max(0, index - 1) : index) : index + 1
        guard starts.indices.contains(target) else { return }
        lastBack = back ? now : nil
        let old = synthesizer
        old.delegate = nil
        old.stopSpeaking()
        synthesizer = NSSpeechSynthesizer(voice: old.voice()) ?? NSSpeechSynthesizer()
        synthesizer.delegate = self
        synthesizer.rate = old.rate
        synthesizer.volume = muted ? 0 : 1
        base = starts[target]
        current = base
        if !synthesizer.startSpeaking(source.substring(from: base)) {
            emit("ERROR\tfailed to restart speech")
            exit(1)
        }
    }
    private var started = false

    func handle(_ command: String) {
        let fields = command.split(separator: "\t", omittingEmptySubsequences: false).map(String.init)
        guard let verb = fields.first else { return }
        switch verb {
        case "SPEAK":
            guard !started, fields.count == 4,
                  let multiplier = Float(fields[3]),
                  multiplier.isFinite, (0.5...2.0).contains(multiplier),
                  let payload = Data(base64Encoded: fields[1]),
                  let message = String(data: payload, encoding: .utf8) else {
                emit("ERROR\tinvalid speech payload")
                exit(1)
            }
            source = message as NSString
            var offset = 0
            for line in message.components(separatedBy: "\n") {
                if offset > 0 && !line.isEmpty { starts.append(offset) }
                offset += (line as NSString).length + 1
            }
            started = true
            synthesizer.delegate = self
            if !fields[2].isEmpty {
                let voice = NSSpeechSynthesizer.VoiceName(rawValue: fields[2])
                if !synthesizer.setVoice(voice) {
                    // Match the existing Codex voice fallback rather than
                    // dropping the entire utterance when a voice was removed.
                    emit("WARN\trequested voice unavailable; using system default")
                }
            }
            // NSSpeechSynthesizer ignores rate updates during an active
            // utterance; set it after voice selection but before playback.
            synthesizer.rate *= multiplier
            synthesizer.volume = muted ? 0 : 1
            if !synthesizer.startSpeaking(message) {
                emit("ERROR\tfailed to start speech")
                exit(1)
            }
        case "MUTE", "UNMUTE":
            muted = verb == "MUTE"
            synthesizer.volume = muted ? 0 : 1
        case "BACK", "NEXT":
            seek(back: verb == "BACK")
        case "PAUSE":
            if started { synthesizer.pauseSpeaking(at: NSSpeechSynthesizer.Boundary(rawValue: 0)!) }
        case "RESUME":
            if started { synthesizer.continueSpeaking() }
        case "RATE":
            if fields.count == 2, let rate = Float(fields[1]), rate > 0 {
                synthesizer.rate = rate
            }
        case "STOP":
            if started { synthesizer.stopSpeaking() }
            exit(0)
        default:
            emit("ERROR\tunknown command")
            exit(1)
        }
    }

    func speechSynthesizer(_ sender: NSSpeechSynthesizer, willSpeakWord range: NSRange, of string: String) {
        guard sender === synthesizer else { return }
        current = base + range.location
        emit("WORD\t\(current)\t\(range.length)")
    }

    func speechSynthesizer(_ sender: NSSpeechSynthesizer, didFinishSpeaking finished: Bool) {
        guard sender === synthesizer else { return }
        emit("DONE\t\(finished)")
        exit(finished ? 0 : 1)
    }
}

private let service = SpeechService()
DispatchQueue.global(qos: .userInitiated).async {
    while let command = readLine() {
        DispatchQueue.main.async { service.handle(command) }
    }
}
RunLoop.main.run()
