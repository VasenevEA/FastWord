import AVFoundation

enum RecorderError: Error {
    case engineStart(String)
    case formatUnavailable
}

final class Recorder {
    var onLevel: ((Float) -> Void)?
    /// Fired (on main) when the audio hardware changes mid-recording (e.g.
    /// the user unplugs headphones). The engine has already been stopped by
    /// the time this fires; the listener should treat the recording as
    /// cancelled.
    var onHardwareChange: (() -> Void)?

    private let engine = AVAudioEngine()
    private var converter: AVAudioConverter?
    private var buffer = Data()
    private let targetSampleRate: Double = 16000
    private let lock = NSLock()
    private var smoothedLevel: Float = 0
    private var configChangeObserver: NSObjectProtocol?
    private var isRecording = false

    func start() throws {
        buffer.removeAll(keepingCapacity: true)
        installConfigChangeObserverIfNeeded()
        let input = engine.inputNode
        let inputFormat = input.outputFormat(forBus: 0)
        guard let outputFormat = AVAudioFormat(
            commonFormat: .pcmFormatFloat32,
            sampleRate: targetSampleRate,
            channels: 1,
            interleaved: false
        ) else {
            throw RecorderError.formatUnavailable
        }
        let converter = AVAudioConverter(from: inputFormat, to: outputFormat)
        self.converter = converter

        input.removeTap(onBus: 0)
        input.installTap(onBus: 0, bufferSize: 4096, format: inputFormat) { [weak self] inBuf, _ in
            guard let self, let converter = self.converter else { return }
            let ratio = outputFormat.sampleRate / inputFormat.sampleRate
            let capacity = AVAudioFrameCount(Double(inBuf.frameLength) * ratio + 1024)
            guard let outBuf = AVAudioPCMBuffer(pcmFormat: outputFormat, frameCapacity: capacity) else { return }
            var error: NSError?
            var supplied = false
            converter.convert(to: outBuf, error: &error) { _, status in
                if supplied {
                    status.pointee = .noDataNow
                    return nil
                }
                supplied = true
                status.pointee = .haveData
                return inBuf
            }
            if error != nil { return }
            guard let channel = outBuf.floatChannelData?[0] else { return }
            let frames = Int(outBuf.frameLength)

            // RMS level for HUD equalizer.
            var sumSq: Float = 0
            for i in 0..<frames {
                let s = channel[i]
                sumSq += s * s
            }
            let rms = sqrtf(sumSq / Float(max(frames, 1)))
            // Speech RMS is typically 0.01–0.08. Apply sqrt curve + heavy gain so
            // quiet talking still drives the bars visibly.
            let normalized = min(1.0, sqrtf(rms) * 3.5)
            self.smoothedLevel = max(self.smoothedLevel * 0.55, normalized)
            let level = self.smoothedLevel
            DispatchQueue.main.async { [weak self] in
                guard let self, self.isRecording else { return }
                self.onLevel?(level)
            }

            self.lock.lock()
            self.buffer.append(UnsafeBufferPointer(start: channel, count: frames).withMemoryRebound(to: UInt8.self) { ptr in
                Data(bytes: ptr.baseAddress!, count: frames * MemoryLayout<Float>.size)
            })
            self.lock.unlock()
        }

        engine.prepare()
        do {
            try engine.start()
            isRecording = true
        } catch {
            throw RecorderError.engineStart(error.localizedDescription)
        }
    }

    /// Subscribe once for the lifetime of the Recorder. macOS posts this
    /// notification when the input device changes (headphones plugged / unplugged,
    /// USB mic dis/connected, sample-rate change, etc). The engine stops itself,
    /// and any taps already installed are torn down. We must not pretend to
    /// keep recording — propagate to the controller so it can cancel cleanly.
    private func installConfigChangeObserverIfNeeded() {
        guard configChangeObserver == nil else { return }
        configChangeObserver = NotificationCenter.default.addObserver(
            forName: .AVAudioEngineConfigurationChange,
            object: engine,
            queue: .main
        ) { [weak self] _ in
            guard let self, self.isRecording else { return }
            // Engine has already been stopped by AVAudioEngine itself.
            self.isRecording = false
            self.lock.lock()
            self.buffer.removeAll(keepingCapacity: false)
            self.lock.unlock()
            self.onHardwareChange?()
        }
    }

    deinit {
        if let observer = configChangeObserver {
            NotificationCenter.default.removeObserver(observer)
        }
    }

    /// Snapshot the audio captured so far without stopping the recording.
    /// Returns at most `maxSeconds` of the most recent audio.
    func snapshot(maxSeconds: Double = 30) -> Data? {
        lock.lock()
        defer { lock.unlock() }
        guard !buffer.isEmpty else { return nil }
        let bytesPerSample = MemoryLayout<Float>.size
        let maxBytes = Int(targetSampleRate * maxSeconds) * bytesPerSample
        if buffer.count > maxBytes {
            return buffer.suffix(maxBytes)
        }
        return buffer
    }

    @discardableResult
    func stop() -> Data? {
        isRecording = false
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
        lock.lock()
        let data = buffer
        buffer.removeAll(keepingCapacity: false)
        lock.unlock()
        return data.isEmpty ? nil : data
    }
}
