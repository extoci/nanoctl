#if DEBUG && targetEnvironment(simulator)
import CoreVideo
import QuartzCore

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
    struct Frame: Encodable { let sequence: Int; let input: Int; let displayMs: Double }
    struct Report: Encodable { let frames: [Frame]; let inputTimes: [Double]; let width: Int; let height: Int }

    private weak var session: Session?
    private let epochOffset = Date().timeIntervalSince1970 - CACurrentMediaTime()
    private var frames: [Frame] = []
    private var inputTimes: [Double] = []
    private var run: Task<Void, Never>?

    init(session: Session) { self.session = session }

    func record(_ marker: FrameMarker, at time: Double, width: Int, height: Int) {
        guard frames.count < 2000 else { return }
        frames.append(Frame(sequence: marker.sequence, input: marker.input, displayMs: (time + epochOffset) * 1000))
        guard run == nil, frames.count >= 30, let session, session.inputReady else { return }
        frames.removeAll(keepingCapacity: true)
        run = Task {
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
            let report = Report(frames: frames, inputTimes: inputTimes, width: width, height: height)
            let file = URL.documentsDirectory.appending(path: "latency.json")
            if let data = try? JSONEncoder().encode(report) { try? data.write(to: file, options: .atomic) }
        }
    }
}
#endif
