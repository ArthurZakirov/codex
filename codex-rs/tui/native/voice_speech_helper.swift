// Word-position-aware macOS speech worker. All AppKit calls stay on the process main thread.
// stdin: SPEAK<TAB>base64(UTF-8)<TAB>voice-id<TAB>rate-multiplier[<TAB>UTF-16 paragraph start], PAUSE, RESUME, STOP
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
    private var wordIndex: [Int: Int] = [:]
    private var words: [NSRange] = []
    private var gapTimer: Timer?
    private var paused = false
    private var lastEmitted = -1

    // AppKit can omit a short word between two real callbacks. Bridge one word
    // within the same phrase. Every native callback takes precedence.
    private func scheduleAdjacentWord(after index: Int) {
        guard !paused, words.indices.contains(index + 1) else { return }
        let word = words[index]
        let next = words[index + 1]
        let gap = source.substring(with: NSRange(location: NSMaxRange(word), length: next.location - NSMaxRange(word)))
        guard !gap.isEmpty, gap.allSatisfy({ $0 == " " || $0 == "\t" }) else { return }
        let seconds = 60 / Double(max(synthesizer.rate, 1))
        let weight = min(1.35, max(0.55, Double(word.length) / 5))
        gapTimer = Timer.scheduledTimer(withTimeInterval: seconds * weight, repeats: false) { [weak self] _ in
            guard let self, !self.paused else { return }
            self.lastEmitted = next.location
            emit("WORD\t\(next.location)\t\(next.length)")
        }
    }
    private var base = 0
    private var current = 0
    private var selectedVoice: NSSpeechSynthesizer.VoiceName?
    private var muted = false

    private func seek(back: Bool) {
        guard started else { return }
        let index = starts.lastIndex(where: { $0 <= current }) ?? 0
        let target = back ? index - 1 : index + 1
        guard starts.indices.contains(target) else { return }
        gapTimer?.invalidate()
        let old = synthesizer
        old.delegate = nil
        old.stopSpeaking()
        guard let replacement = NSSpeechSynthesizer(voice: selectedVoice) else {
            emit("ERROR\tselected speech voice unavailable")
            exit(1)
        }
        synthesizer = replacement
        synthesizer.delegate = self
        synthesizer.rate = old.rate
        synthesizer.volume = muted ? 0 : 1
        lastEmitted = -1
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
            guard !started, (4...5).contains(fields.count),
                  let multiplier = Float(fields[3]),
                  multiplier.isFinite, (0.5...2.0).contains(multiplier),
                  let payload = Data(base64Encoded: fields[1]),
                  let message = String(data: payload, encoding: .utf8) else {
                emit("ERROR\tinvalid speech payload")
                exit(1)
            }
            source = message as NSString
            message.enumerateSubstrings(in: message.startIndex..<message.endIndex, options: [.byWords, .substringNotRequired]) { _, range, _, _ in
                let word = NSRange(range, in: message)
                self.wordIndex[word.location] = self.words.count
                self.words.append(word)
            }
            var offset = 0
            for line in message.components(separatedBy: "\n") {
                if offset > 0 && !line.isEmpty { starts.append(offset) }
                offset += (line as NSString).length + 1
            }
            guard let start = Int(fields.count == 5 ? fields[4] : "0"), starts.contains(start) else {
                emit("ERROR\tinvalid paragraph start")
                exit(1)
            }
            base = start
            current = start
            started = true
            synthesizer.delegate = self
            if !fields[2].isEmpty {
                let voice = NSSpeechSynthesizer.VoiceName(rawValue: fields[2])
                selectedVoice = voice
                if !synthesizer.setVoice(voice) {
                    emit("ERROR\tconfigured speech voice unavailable")
                    exit(1)
                }
            }
            // NSSpeechSynthesizer ignores rate updates during an active
            // utterance; set it after voice selection but before playback.
            synthesizer.rate *= multiplier
            synthesizer.volume = muted ? 0 : 1
            if !synthesizer.startSpeaking(source.substring(from: base)) {
                emit("ERROR\tfailed to start speech")
                exit(1)
            }
        case "MUTE", "UNMUTE":
            muted = verb == "MUTE"
            synthesizer.volume = muted ? 0 : 1
        case "BACK", "NEXT":
            seek(back: verb == "BACK")
        case "PAUSE":
            paused = true
            gapTimer?.invalidate()
            if started { synthesizer.pauseSpeaking(at: NSSpeechSynthesizer.Boundary(rawValue: 0)!) }
        case "RESUME":
            paused = false
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
        let reported = base + range.location
        // Some system voices include the preceding space in their word offset.
        // AppKit may still report the length of the word without that space.
        var first = reported
        while first < source.length && first < reported + range.length {
            if wordIndex[first] != nil { break }
            guard let scalar = UnicodeScalar(source.character(at: first)),
                  CharacterSet.whitespacesAndNewlines.union(.punctuationCharacters).contains(scalar)
            else { break }
            first += 1
        }
        guard first >= lastEmitted else { return }
        gapTimer?.invalidate()
        let index = wordIndex[first]
        let word = index.map { words[$0] }
        current = word?.location ?? reported
        lastEmitted = current
        emit("WORD\t\(current)\t\(word?.length ?? range.length)")
        if let index { scheduleAdjacentWord(after: index) }
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
