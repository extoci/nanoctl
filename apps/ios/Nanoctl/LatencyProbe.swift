#if DEBUG && targetEnvironment(simulator)
import CoreVideo
import QuartzCore
import WebRTC

final class DecoderTimings {
    struct Sample: Encodable {
        let start: Double
        var end: Double?
    }

    static let shared = DecoderTimings()
    private let lock = NSLock()
    private var samples: [UInt32: Sample] = [:]
    private var order: [UInt32] = []

    func start(_ rtp: UInt32) {
        let now = CACurrentMediaTime()
        lock.withLock {
            if samples[rtp] == nil { order.append(rtp) }
            if order.count > 128 { samples.removeValue(forKey: order.removeFirst()) }
            samples[rtp] = Sample(start: now)
        }
    }

    func end(_ rtp: UInt32) {
        let now = CACurrentMediaTime()
        lock.withLock { samples[rtp]?.end = now }
    }

    func timing(for rtp: UInt32) -> Sample? { lock.withLock { samples[rtp] } }
}

final class TimedDecoderFactory: NSObject, RTCVideoDecoderFactory {
    private let factory = RTCDefaultVideoDecoderFactory()

    func supportedCodecs() -> [RTCVideoCodecInfo] { factory.supportedCodecs() }

    func createDecoder(_ info: RTCVideoCodecInfo) -> RTCVideoDecoder? {
        factory.createDecoder(info).map(TimedDecoder.init)
    }
}

private final class TimedDecoder: NSObject, RTCVideoDecoder {
    private let decoder: RTCVideoDecoder

    init(_ decoder: RTCVideoDecoder) { self.decoder = decoder }

    func setCallback(_ callback: @escaping RTCVideoDecoderCallback) {
        decoder.setCallback { frame in
            DecoderTimings.shared.end(UInt32(bitPattern: frame.timeStamp))
            callback(frame)
        }
    }

    func startDecode(withNumberOfCores numberOfCores: Int32) -> Int {
        decoder.startDecode(withNumberOfCores: numberOfCores)
    }

    func release() -> Int { decoder.release() }

    func implementationName() -> String { decoder.implementationName() }

    func decode(_ image: RTCEncodedImage, missingFrames: Bool, codecSpecificInfo info: RTCCodecSpecificInfo?,
                renderTimeMs: Int64) -> Int {
        DecoderTimings.shared.start(image.timeStamp)
        return decoder.decode(image, missingFrames: missingFrames, codecSpecificInfo: info, renderTimeMs: renderTimeMs)
    }
}

struct FrameMarker {
    let sequence: Int
    let input: Int

    static func read(_ buffer: CVPixelBuffer) -> Self? {
        guard CVPixelBufferLockBaseAddress(buffer, .readOnly) == kCVReturnSuccess else { return nil }
        defer { CVPixelBufferUnlockBaseAddress(buffer, .readOnly) }
        let width = CVPixelBufferGetWidth(buffer)
        let height = CVPixelBufferGetHeight(buffer)
        let planes = CVPixelBufferGetPlaneCount(buffer)
        let format = CVPixelBufferGetPixelFormatType(buffer)
        for row in [height / 2, height / 4, 3 * height / 4] {
            if planes == 0, format == kCVPixelFormatType_32BGRA, let base = CVPixelBufferGetBaseAddress(buffer) {
                let pixels = base.advanced(by: row * CVPixelBufferGetBytesPerRow(buffer)).assumingMemoryBound(to: UInt8.self)
                if let marker = scan(width: width, sample: { x in
                    (Int(pixels[x * 4 + 2]), Int(pixels[x * 4 + 1]), Int(pixels[x * 4]))
                }) { return marker }
            } else if (planes == 2 || planes == 3),
                      let y = CVPixelBufferGetBaseAddressOfPlane(buffer, 0),
                      let u = CVPixelBufferGetBaseAddressOfPlane(buffer, 1) {
                let luma = y.advanced(by: row * CVPixelBufferGetBytesPerRowOfPlane(buffer, 0)).assumingMemoryBound(to: UInt8.self)
                let chroma = u.advanced(by: row / 2 * CVPixelBufferGetBytesPerRowOfPlane(buffer, 1)).assumingMemoryBound(to: UInt8.self)
                let v = planes == 3 ? CVPixelBufferGetBaseAddressOfPlane(buffer, 2)?
                    .advanced(by: row / 2 * CVPixelBufferGetBytesPerRowOfPlane(buffer, 2)).assumingMemoryBound(to: UInt8.self) : nil
                if let marker = scan(width: width, sample: { x in
                    let cb = Double(chroma[v == nil ? x & ~1 : x / 2]) - 128
                    let cr = Double(v?[x / 2] ?? chroma[(x & ~1) + 1]) - 128
                    let y = 1.164 * (Double(luma[x]) - 16)
                    return (Int(y + 1.596 * cr), Int(y - 0.392 * cb - 0.813 * cr), Int(y + 2.017 * cb))
                }) { return marker }
            }
        }
        return nil
    }

    private static func scan(width: Int, sample: (Int) -> (Int, Int, Int)) -> Self? {
        var left = -1, right = -1
        for x in 0..<width {
            let (r, g, b) = sample(x)
            if r < 80 && g > 150 && b > 150 { left = x }
            if left >= 0 && r > 150 && g < 80 && b > 150 { right = x; break }
        }
        guard left >= 0, right - left >= 144 else { return nil }
        func bit(_ i: Int) -> Int {
            let (r, _, _) = sample(left + (right - left) * (2 * i + 1) / 72)
            return r > 192 ? 1 : r < 64 ? 0 : -1
        }
        guard bit(0) == 1, bit(1) == 0, bit(34) == 0, bit(35) == 1 else { return nil }
        var word = 0
        for i in 2..<34 {
            let value = bit(i)
            guard value >= 0 else { return nil }
            word = (word << 1) | value
        }
        let sequence = word >> 16, input = (word >> 8) & 255
        guard (word & 255) == (((sequence >> 8) ^ sequence ^ input ^ 0xa5) & 255) else { return nil }
        return Self(sequence: sequence, input: input)
    }
}

@MainActor
final class LatencyProbe {
    struct Stages: Encodable {
        let rtp: UInt32
        let decoder: DecoderTimings.Sample?
        let mailbox: Double
        let renderStart: Double
        let submit: Double
        let gpuEnd: Double
    }
    struct Frame: Encodable {
        let sequence: Int
        let input: Int
        let displayMs: Double
        let stages: Stages
    }
    struct Report: Encodable {
        let frames: [Frame]
        let inputTimes: [Double]
        let width: Int
        let height: Int
        let epochOffset: Double
        let receiverStart: ReceiverStats?
        let receiverEnd: ReceiverStats?
    }

    static let enabled = ProcessInfo.processInfo.environment["NANOCTL_BENCHMARK_URL"] != nil
    let vsyncBaseline = ProcessInfo.processInfo.environment["NANOCTL_RENDER_ON_VSYNC"] == "1"
    private weak var session: Session?
    private let epochOffset = Date().timeIntervalSince1970 - CACurrentMediaTime()
    private var frames: [Frame] = []
    private var inputTimes: [Double] = []
    private var run: Task<Void, Never>?

    init(session: Session) { self.session = session }

    func record(_ marker: FrameMarker, at time: Double, width: Int, height: Int, stages: Stages) {
        guard frames.count < 2000 else { return }
        frames.append(Frame(sequence: marker.sequence, input: marker.input,
                            displayMs: (time + epochOffset) * 1000, stages: stages))
        guard run == nil, frames.count >= 30, let session, session.inputReady else { return }
        frames.removeAll(keepingCapacity: true)
        run = Task {
            let receiverStart = await session.receiverStats()
            let start = CACurrentMediaTime()
            for i in 1...12 {
                try? await Task.sleep(for: .milliseconds(181 + (i * 43) % 193))
                let time = (CACurrentMediaTime() + epochOffset) * 1000
                guard session.sendKey(true) else { break }
                inputTimes.append(time)
                try? await Task.sleep(for: .milliseconds(30))
                session.sendKey(false)
                let deadline = CACurrentMediaTime() + 3
                while frames.last?.input != i && CACurrentMediaTime() < deadline {
                    try? await Task.sleep(for: .milliseconds(10))
                }
            }
            try? await Task.sleep(for: .seconds(max(0, 10 - (CACurrentMediaTime() - start))))
            let receiverEnd = await session.receiverStats()
            let report = Report(frames: frames, inputTimes: inputTimes, width: width, height: height,
                                epochOffset: epochOffset, receiverStart: receiverStart, receiverEnd: receiverEnd)
            let file = URL.documentsDirectory.appending(path: "latency.json")
            if let data = try? JSONEncoder().encode(report) { try? data.write(to: file, options: .atomic) }
        }
    }
}

struct ReceiverStats: Encodable {
    let decoder: String?
    let ssrc: UInt32?
    let powerEfficientDecoder: Bool?
    let counters: [String: Double]

    init(_ values: [String: NSObject]) {
        decoder = values["decoderImplementation"] as? String
        ssrc = (values["ssrc"] as? NSNumber)?.uint32Value
        powerEfficientDecoder = (values["powerEfficientDecoder"] as? NSNumber)?.boolValue
        counters = Dictionary(uniqueKeysWithValues: [
            "jitterBufferDelay", "jitterBufferTargetDelay", "jitterBufferMinimumDelay", "jitterBufferEmittedCount",
            "totalDecodeTime", "totalProcessingDelay", "framesDecoded", "framesReceived", "framesDropped"
        ].compactMap { key in (values[key] as? NSNumber).map { (key, $0.doubleValue) } })
    }
}
#endif
