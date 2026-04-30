import AVFoundation

enum RecorderError: Error {
    case engineStart(String)
    case formatUnavailable
}

final class Recorder {
    var onLevel: ((Float) -> Void)?

    private let engine = AVAudioEngine()
    private var converter: AVAudioConverter?
    private var buffer = Data()
    private let targetSampleRate: Double = 16000
    private let lock = NSLock()
    private var smoothedLevel: Float = 0

    func start() throws {
        buffer.removeAll(keepingCapacity: true)
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
            // Map RMS (~0..0.3 for normal speech) to 0..1, exponential smoothing.
            let normalized = min(1.0, rms * 6.0)
            self.smoothedLevel = self.smoothedLevel * 0.6 + normalized * 0.4
            let level = self.smoothedLevel
            DispatchQueue.main.async { [weak self] in self?.onLevel?(level) }

            self.lock.lock()
            self.buffer.append(UnsafeBufferPointer(start: channel, count: frames).withMemoryRebound(to: UInt8.self) { ptr in
                Data(bytes: ptr.baseAddress!, count: frames * MemoryLayout<Float>.size)
            })
            self.lock.unlock()
        }

        engine.prepare()
        do {
            try engine.start()
        } catch {
            throw RecorderError.engineStart(error.localizedDescription)
        }
    }

    @discardableResult
    func stop() -> Data? {
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
        lock.lock()
        let data = buffer
        buffer.removeAll(keepingCapacity: false)
        lock.unlock()
        return data.isEmpty ? nil : data
    }
}
