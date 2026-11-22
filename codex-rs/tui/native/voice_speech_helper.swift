// Word-position-aware macOS speech worker. All AppKit calls stay on the process main thread.
// stdin: SPEAK<TAB>base64(UTF-8)<TAB>voice-id<TAB>rate-multiplier[<TAB>UTF-16 paragraph start], PAUSE, RESUME, STOP
// stdout: WORD<TAB>utf16-start<TAB>utf16-length, DONE, ERROR<TAB>reason
import AVFoundation
import AudioToolbox
import AppKit
import Foundation

private func emit(_ message: String) {
    print(message)
    fflush(stdout)
}

private final class SpeechService: NSObject, NSSpeechSynthesizerDelegate, AVAudioPlayerDelegate {
    private var synthesizer = NSSpeechSynthesizer()
    private var source: NSString = ""
    private var starts = [0]
    // Render one paragraph at a time. The live NSSpeech output can finish after
    // a few words even though the same voice renders the entire paragraph to file.
    private var player: AVAudioPlayer?
    private var playbackTimer: Timer?
    private var paused = false
    private var audioURL: URL?
    private var cues: [(range: NSRange, fileOffset: Int64)] = []
    private var cueIndex = 0
    private var dataOffset: Int64 = 0
    private var bytesPerSecond: Double = 1

    private func clearPlayback() {
        playbackTimer?.invalidate()
        playbackTimer = nil
        player?.stop()
        player = nil
        if let url = audioURL { try? FileManager.default.removeItem(at: url) }
        audioURL = nil
        cues.removeAll(keepingCapacity: true)
        cueIndex = 0
    }

    private func renderParagraph(at index: Int) {
        clearPlayback()
        guard starts.indices.contains(index) else {
            emit("DONE\ttrue")
            exit(0)
        }
        base = starts[index]
        current = base
        let end = index + 1 < starts.count ? starts[index + 1] : source.length
        let paragraph = source.substring(with: NSRange(location: base, length: end - base))
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("codex-speech-\(UUID().uuidString).aiff")
        // The renderer writes dictated content; make the temporary file owner-only.
        guard FileManager.default.createFile(atPath: url.path, contents: nil, attributes: [.posixPermissions: 0o600]) else {
            emit("ERROR\tcannot create temporary speech audio")
            exit(1)
        }
        audioURL = url
        // A right arrow causes some macOS voices to report the whole sentence
        // as one word. A comma keeps the pause and the original UTF-16 offsets.
        let narration = paragraph.replacingOccurrences(of: "→", with: ",")
        if !synthesizer.startSpeaking(narration, to: url) {
            clearPlayback()
            emit("ERROR\tcannot render speech audio")
            exit(1)
        }
    }

    // Some voices return one range for a whole phrase. Keep native boundaries
    // where available and estimate the interior word times over its audio span.
    private func expandPhraseCues(audioEnd: Int64) {
        let native = cues
        cues.removeAll(keepingCapacity: true)
        for (index, cue) in native.enumerated() {
            let phrase = source.substring(with: cue.range)
            var words: [NSRange] = []
            phrase.enumerateSubstrings(in: phrase.startIndex..<phrase.endIndex, options: [.byWords, .substringNotRequired]) { _, range, _, _ in
                words.append(NSRange(range, in: phrase))
            }
            guard words.count > 1 else {
                cues.append(cue)
                continue
            }
            let start = max(dataOffset, cue.fileOffset)
            let end = index + 1 < native.count ? native[index + 1].fileOffset : audioEnd
            for word in words {
                let fraction = Double(word.location) / Double(cue.range.length)
                let offset = start + Int64(Double(max(0, end - start)) * fraction)
                cues.append((NSRange(location: cue.range.location + word.location, length: word.length), offset))
            }
        }
    }

    private func updatePlayback() {
        guard let player, !paused else { return }
        let offset = dataOffset + Int64(player.currentTime * bytesPerSecond)
        while cueIndex < cues.count && cues[cueIndex].fileOffset <= offset {
            let word = cues[cueIndex].range
            current = word.location
            emit("WORD\t\(word.location)\t\(word.length)")
            cueIndex += 1
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
        clearPlayback()
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
        renderParagraph(at: target)
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
            renderParagraph(at: starts.firstIndex(of: start)!)
        case "MUTE", "UNMUTE":
            muted = verb == "MUTE"
            player?.volume = muted ? 0 : 1
        case "BACK", "NEXT":
            seek(back: verb == "BACK")
        case "PAUSE":
            paused = true
            player?.pause()
        case "RESUME":
            paused = false
            if let player, !player.isPlaying { _ = player.play() }
        case "RATE":
            if fields.count == 2, let rate = Float(fields[1]), rate > 0 {
                synthesizer.rate = rate
            }
        case "STOP":
            synthesizer.delegate = nil
            if started { synthesizer.stopSpeaking() }
            clearPlayback()
            exit(0)
        default:
            emit("ERROR\tunknown command")
            exit(1)
        }
    }

    func speechSynthesizer(_ sender: NSSpeechSynthesizer, willSpeakWord range: NSRange, of string: String) {
        guard sender === synthesizer else { return }
        guard let url = audioURL, range.location != NSNotFound, range.length > 0,
              NSMaxRange(range) <= source.length - base,
              let size = try? url.resourceValues(forKeys: [.fileSizeKey]).fileSize else { return }
        cues.append((NSRange(location: base + range.location, length: range.length), Int64(size)))
    }

    func speechSynthesizer(_ sender: NSSpeechSynthesizer, didFinishSpeaking finished: Bool) {
        guard sender === synthesizer, let url = audioURL else { return }
        do {
            guard finished else { throw CocoaError(.fileReadUnknown) }
            let file = try AVAudioFile(forReading: url)
            let format = file.fileFormat.streamDescription.pointee
            bytesPerSecond = format.mSampleRate * Double(format.mBytesPerFrame)
            var audioFile: AudioFileID?
            guard bytesPerSecond > 0,
                  AudioFileOpenURL(url as CFURL, .readPermission, 0, &audioFile) == noErr,
                  let audioFile else { throw CocoaError(.fileReadCorruptFile) }
            defer { AudioFileClose(audioFile) }
            var size = UInt32(MemoryLayout<Int64>.size)
            guard AudioFileGetProperty(audioFile, kAudioFilePropertyDataOffset, &size, &dataOffset) == noErr else {
                throw CocoaError(.fileReadCorruptFile)
            }
            let output = try AVAudioPlayer(contentsOf: url)
            expandPhraseCues(audioEnd: dataOffset + Int64(output.duration * bytesPerSecond))
            player = output
            output.delegate = self
            output.volume = muted ? 0 : 1
            guard output.prepareToPlay(), paused || output.play() else { throw CocoaError(.fileReadUnknown) }
            try? FileManager.default.removeItem(at: url)
            audioURL = nil
            playbackTimer = Timer.scheduledTimer(withTimeInterval: 0.015, repeats: true) { [weak self] _ in
                self?.updatePlayback()
            }
        } catch {
            clearPlayback()
            emit("ERROR\tcould not play rendered speech: \(error.localizedDescription)")
            exit(1)
        }
    }

    func audioPlayerDidFinishPlaying(_ player: AVAudioPlayer, successfully flag: Bool) {
        guard player === self.player else { return }
        guard flag else {
            clearPlayback()
            emit("ERROR\taudio output interrupted")
            exit(1)
        }
        let next = (starts.firstIndex(of: base) ?? 0) + 1
        renderParagraph(at: next)
    }

}

private let service = SpeechService()
DispatchQueue.global(qos: .userInitiated).async {
    while let command = readLine() {
        DispatchQueue.main.async { service.handle(command) }
    }
}
RunLoop.main.run()
