import CoreMedia
import CoreVideo
import Foundation
import OSLog
import VideoToolbox

private let log = Logger(subsystem: "ai.nanoctl.viewer", category: "vtdecoder")

/// Low-latency VideoToolbox decoder fed whole Annex-B access units from the
/// `nanoctl.video.v1` data channels — the direct equivalent of the browser
/// viewer's WebCodecs `latencyMode: "realtime"` path, with no jitter buffer.
///
/// The decoder configures lazily from the first access unit carrying the
/// codec's parameter sets and reconfigures whenever they change (resize,
/// encoder reset). Decode errors invalidate the session and surface `onFault`
/// so the caller can request a fresh keyframe.
final class VTDecoder {
    enum Codec: String {
        case h264
        case hevc
    }

    let codec: Codec
    /// Latest decoded frame, called on the decode callback queue.
    var onFrame: ((CVPixelBuffer) -> Void)?
    /// Decoder or bitstream failure; caller should drop deltas until a keyframe.
    var onFault: (() -> Void)?

    private(set) var decoded = 0
    private(set) var dropped = 0
    private(set) var faults = 0

    private var session: VTDecompressionSession?
    private var format: CMFormatDescription?
    private var parameters: [Data] = []
    private var inflight = 0
    private let maxInflight = 3

    init(codec: Codec) { self.codec = codec }

    deinit {
        if let session { VTDecompressionSessionInvalidate(session) }
    }

    /// Feeds one Annex-B access unit. `seq` drives presentation ordering and
    /// timestamps, mirroring the browser's `seq * 16_000` microseconds.
    func decode(_ au: Data, isKey: Bool, seq: UInt32) {
        var parameters: [Data] = []
        guard let avcc = Self.avcc(au, codec: codec, parameters: &parameters) else {
            dropped += 1
            return
        }
        if session == nil {
            guard ensureSession(parameters: parameters) else {
                let head = au.prefix(12).map { String(format: "%02x", $0) }.joined()
                log.error("no session: \(parameters.count, privacy: .public) param sets, codec=\(self.codec.rawValue, privacy: .public), head=\(head, privacy: .public), isKey=\(isKey, privacy: .public)")
                if isKey { fault() }
                dropped += 1
                return
            }
        } else if !parameters.isEmpty && parameters != self.parameters {
            // Parameter sets changed (resize/encoder reset): rebuild on the spot.
            invalidate()
            guard ensureSession(parameters: parameters) else {
                fault()
                return
            }
        }
        if inflight > maxInflight && !isKey {
            dropped += 1
            return
        }
        var block: CMBlockBuffer?
        guard CMBlockBufferCreateWithMemoryBlock(
            allocator: kCFAllocatorDefault, memoryBlock: nil, blockLength: avcc.count,
            blockAllocator: kCFAllocatorDefault, customBlockSource: nil,
            offsetToData: 0, dataLength: avcc.count, flags: 0, blockBufferOut: &block
        ) == kCMBlockBufferNoErr, let block else { return }
        let copy = avcc.withUnsafeBytes {
            CMBlockBufferReplaceDataBytes(with: $0.baseAddress!, blockBuffer: block,
                                        offsetIntoDestination: 0, dataLength: avcc.count)
        }
        guard copy == kCMBlockBufferNoErr, let format else { return }

        var timing = CMSampleTimingInfo(
            duration: CMTime.invalid,
            presentationTimeStamp: CMTime(value: Int64(seq) * 16_000, timescale: 1_000_000),
            decodeTimeStamp: CMTime.invalid
        )
        var sizes = [avcc.count]
        var sample: CMSampleBuffer?
        guard CMSampleBufferCreate(
            allocator: kCFAllocatorDefault, dataBuffer: block, dataReady: true,
            makeDataReadyCallback: nil, refcon: nil, formatDescription: format,
            sampleCount: 1, sampleTimingEntryCount: 1, sampleTimingArray: &timing,
            sampleSizeEntryCount: 1, sampleSizeArray: &sizes, sampleBufferOut: &sample
        ) == noErr, let sample else { return }
        // Present every frame as soon as it is decoded — never hold for reorder.
        if let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: true),
           CFArrayGetCount(attachments) > 0 {
            let first = unsafeBitCast(CFArrayGetValueAtIndex(attachments, 0),
                                      to: CFMutableDictionary.self)
            CFDictionarySetValue(first,
                Unmanaged.passUnretained(kCMSampleAttachmentKey_DisplayImmediately).toOpaque(),
                Unmanaged.passUnretained(kCFBooleanTrue).toOpaque())
        }

        inflight += 1
        let status = VTDecompressionSessionDecodeFrame(
            session!, sampleBuffer: sample,
            flags: [._EnableAsynchronousDecompression, ._1xRealTimePlayback],
            infoFlagsOut: nil
        ) { [weak self] status, _, buffer, _, _ in
            guard let self else { return }
            self.inflight -= 1
            guard status == noErr else {
                log.error("decode callback status=\(status, privacy: .public)")
                self.fault()
                return
            }
            guard let buffer else { return }
            self.decoded += 1
            self.onFrame?(buffer)
        }
        if status != noErr {
            inflight -= 1
            fault()
        }
    }

    func invalidate() {
        if let session { VTDecompressionSessionInvalidate(session) }
        session = nil
        format = nil
        parameters = []
        inflight = 0
    }

    private func fault() {
        faults += 1
        invalidate()
        onFault?()
    }

    private func ensureSession(parameters: [Data]) -> Bool {
        // Reuse the stream's own parameter sets; fall back to the previous ones
        // when this access unit carried none.
        let sets = parameters.isEmpty ? self.parameters : parameters
        guard !sets.isEmpty, let format = Self.format(codec: codec, sets: sets) else {
            return false
        }
        self.format = format
        self.parameters = sets
        var session: VTDecompressionSession?
        let attributes: [CFString: Any] = [
            kCVPixelBufferMetalCompatibilityKey: true,
        ]
        let status = VTDecompressionSessionCreate(
            allocator: kCFAllocatorDefault, formatDescription: format,
            decoderSpecification: nil, imageBufferAttributes: attributes as CFDictionary,
            outputCallback: nil, decompressionSessionOut: &session
        )
        guard status == noErr, let session else {
            log.error("session create status=\(status, privacy: .public)")
            return false
        }
        VTSessionSetProperty(session, key: kVTDecompressionPropertyKey_RealTime, value: kCFBooleanTrue)
        self.session = session
        return true
    }

    /// Builds a format description from the codec's parameter sets.
    private static func format(codec: Codec, sets: [Data]) -> CMFormatDescription? {
        var format: CMFormatDescription?
        log.info("format: \(sets.count, privacy: .public) sets")
        let status = sets.withParameterSets { pointers, sizes in
            switch codec {
            case .h264:
                guard sets.count >= 2 else { return OSStatus(-1) }
                return CMVideoFormatDescriptionCreateFromH264ParameterSets(
                    allocator: kCFAllocatorDefault, parameterSetCount: sets.count,
                    parameterSetPointers: pointers, parameterSetSizes: sizes,
                    nalUnitHeaderLength: 4, formatDescriptionOut: &format)
            case .hevc:
                guard sets.count >= 3 else { return OSStatus(-1) }
                return CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                    allocator: kCFAllocatorDefault, parameterSetCount: sets.count,
                    parameterSetPointers: pointers, parameterSetSizes: sizes,
                    nalUnitHeaderLength: 4, extensions: nil, formatDescriptionOut: &format)
            }
        }
        if status != noErr { log.error("format create status=\(status, privacy: .public)") }
        return status == noErr ? format : nil
    }

    /// Splits an Annex-B access unit into its NAL units.
    static func annexBNals(_ au: Data) -> [Data] {
        var nals: [Data] = []
        au.withUnsafeBytes { raw in
            let bytes = raw.bindMemory(to: UInt8.self)
            var i = 0
            var start: Int?
            while i + 3 < bytes.count {
                let isStart = bytes[i] == 0 && bytes[i + 1] == 0 &&
                    (bytes[i + 2] == 1 || (bytes[i + 2] == 0 && i + 3 < bytes.count && bytes[i + 3] == 1))
                if isStart {
                    let head = bytes[i + 2] == 1 ? 3 : 4
                    if let from = start, i > from {
                        nals.append(Data(bytes[from..<i]))
                    }
                    start = i + head
                    i += head
                } else {
                    i += 1
                }
            }
            if let from = start, bytes.count > from {
                nals.append(Data(bytes[from..<bytes.count]))
            }
        }
        return nals
    }

    /// Sniffs the codec from an access unit's own parameter-set NAL headers.
    /// SDP negotiation only says which codecs were offered; the bitstream does
    /// not lie. Parameter-set types are disjoint between the two codecs, so a
    /// match on one side rules the other out.
    static func detectCodec(_ au: Data) -> Codec? {
        var h264 = false
        var hevc = false
        for nal in annexBNals(au) where !nal.isEmpty {
            let header = nal[nal.startIndex]
            switch header & 0x1f {
            case 7, 8: h264 = true // H.264 SPS/PPS
            default: break
            }
            switch (header >> 1) & 0x3f {
            case 32, 33, 34: hevc = true // HEVC VPS/SPS/PPS
            default: break
            }
        }
        if hevc && !h264 { return .hevc }
        if h264 { return .h264 }
        return nil
    }

    /// Converts an Annex-B access unit to the 4-byte length-prefixed framing
    /// VTDecompressionSession consumes, collecting parameter-set NAL units.
    static func avcc(_ au: Data, codec: Codec, parameters: inout [Data]) -> Data? {
        let nals = annexBNals(au)
        guard !nals.isEmpty else { return nil }
        for nal in nals where !nal.isEmpty {
            let type = codec == .h264 ? nal[nal.startIndex] & 0x1f : (nal[nal.startIndex] >> 1) & 0x3f
            let wanted = codec == .h264 ? type == 7 || type == 8 : type == 32 || type == 33 || type == 34
            if wanted {
                parameters.removeAll { existing in
                    codec == .h264 ? existing[existing.startIndex] & 0x1f == type
                        : (existing[existing.startIndex] >> 1) & 0x3f == type
                }
                parameters.append(nal)
            }
        }
        var out = Data()
        out.reserveCapacity(au.count + nals.count)
        var length: UInt32 = 0
        for nal in nals {
            length = UInt32(nal.count).bigEndian
            withUnsafeBytes(of: &length) { out.append(contentsOf: $0) }
            out.append(nal)
        }
        return out
    }
}

private extension Array where Element == Data {
    /// Calls `body` with the parameter-set pointers and sizes, ordered as
    /// CoreMedia expects (vps, sps, pps for HEVC; sps, pps for H.264).
    func withParameterSets<T>(
        _ body: (UnsafePointer<UnsafePointer<UInt8>>, UnsafePointer<Int>) -> T
    ) -> T {
        var pointers: [UnsafePointer<UInt8>] = []
        var sizes: [Int] = []
        func run(_ remaining: ArraySlice<Data>) -> T {
            if remaining.isEmpty {
                return pointers.withUnsafeBufferPointer { p in
                    sizes.withUnsafeBufferPointer { s in
                        body(p.baseAddress!, s.baseAddress!)
                    }
                }
            }
            return remaining.first!.withUnsafeBytes { raw in
                pointers.append(raw.bindMemory(to: UInt8.self).baseAddress!)
                sizes.append(remaining.first!.count)
                defer { pointers.removeLast(); sizes.removeLast() }
                return run(remaining.dropFirst())
            }
        }
        return run(self[...])
    }
}
