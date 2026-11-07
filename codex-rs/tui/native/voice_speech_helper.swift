// Word-position-aware macOS speech worker. All AppKit calls stay on the process main thread.
// stdin: SPEAK<TAB>base64(UTF-8)<TAB>voice-id, PAUSE, RESUME, STOP, RATE<TAB>words-per-minute
// stdout: WORD<TAB>utf16-start<TAB>utf16-length, DONE, ERROR<TAB>reason
import AppKit
import Foundation

private func emit(_ message: String) {
    print(message)
    fflush(stdout)
}

private final class SpeechService: NSObject, NSSpeechSynthesizerDelegate {
    private let synthesizer = NSSpeechSynthesizer()
    private var started = false

    func handle(_ command: String) {
        let fields = command.split(separator: "\t", omittingEmptySubsequences: false).map(String.init)
        guard let verb = fields.first else { return }
        switch verb {
        case "SPEAK":
            guard !started, fields.count == 3,
                  let payload = Data(base64Encoded: fields[1]),
                  let message = String(data: payload, encoding: .utf8) else {
                emit("ERROR\tinvalid speech payload")
                exit(1)
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
            if !synthesizer.startSpeaking(message) {
                emit("ERROR\tfailed to start speech")
                exit(1)
            }
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
        emit("WORD\t\(range.location)\t\(range.length)")
    }

    func speechSynthesizer(_ sender: NSSpeechSynthesizer, didFinishSpeaking finished: Bool) {
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
