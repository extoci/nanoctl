import CoreMedia
import CoreVideo
import Foundation
import OSLog
import VideoToolbox

private let log = AppLogger(subsystem: "ai.nanoctl.viewer", category: "vtdecoder")

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
        case av1
    }

    let codec: Codec
    /// Latest decoded frame, called on the decode callback queue.
    var onFrame: ((CVPixelBuffer) -> Void)?
    /// Decoder or bitstream failure; caller should drop deltas until a keyframe.
    var onFault: (() -> Void)?

    private let stateLock = NSLock()
    private var faultPending = false
    private var decodedCount = 0
    private var droppedCount = 0
    private var faultCount = 0

    private var session: VTDecompressionSession?
    private var av1Core: VTAV1CoreDecoder?
    private var format: CMFormatDescription?
    private var parameters: [Data] = []
    private var inflight = 0
    private var generation = 0
    private let maxInflight = 3

    var decoded: Int { stateLock.withLock { decodedCount } }
    var dropped: Int { stateLock.withLock { droppedCount } }
    var faults: Int { stateLock.withLock { faultCount } }

    init(codec: Codec) { self.codec = codec }

    deinit {
        if let session { VTDecompressionSessionInvalidate(session) }
    }

    /// Feeds one Annex-B access unit. `seq` drives presentation ordering and
    /// timestamps, mirroring the browser's `seq * 16_000` microseconds.
    func decode(_ au: Data, isKey: Bool, seq: UInt32) {
        if takeCallbackFault() { fault() }
        if codec == .av1 {
            if av1Core == nil {
                let core = VTAV1CoreDecoder(maxInflight: maxInflight)
                core.onFrame = { [weak self] buffer, _ in
                    guard let self else { return }
                    self.stateLock.withLock { self.decodedCount += 1 }
                    self.onFrame?(buffer)
                }
                core.onFault = { [weak self] in
                    guard let self else { return }
                    self.stateLock.withLock { self.faultCount += 1 }
                    self.onFault?()
                }
                av1Core = core
            }
            if av1Core?.decode(au, timestamp: Int64(seq) * 16_000,
                               timescale: 1_000_000, isKey: isKey) != true {
                stateLock.withLock { droppedCount += 1 }
            }
            return
        }
        var parameters: [Data] = []
        guard let avcc = Self.avcc(au, codec: codec, parameters: &parameters) else {
            stateLock.withLock { droppedCount += 1 }
            return
        }
        if session == nil {
            guard ensureSession(parameters: parameters) else {
                let head = au.prefix(12).map { String(format: "%02x", $0) }.joined()
                log.error("no session: \(parameters.count) param sets, codec=\(self.codec.rawValue), head=\(head), isKey=\(isKey)")
                if isKey { fault() }
                stateLock.withLock { droppedCount += 1 }
                return
            }
        } else if !parameters.isEmpty && sortedHex(parameters) != sortedHex(self.parameters) {
            // Parameter sets changed (resize/encoder reset): rebuild on the spot.
            // The comparison is order-independent because the encoder does not
            // guarantee stable VPS/SPS/PPS ordering inside the access unit, and
            // an order-only difference is not a format change — rebuilding the
            // decode session every keyframe was a once-a-second visible hitch.
            invalidate()
            guard ensureSession(parameters: parameters) else {
                fault()
                return
            }
        }
        if inflightCount() > maxInflight && !isKey {
            stateLock.withLock { droppedCount += 1 }
            // The dropped delta is a reference-frame loss. Decode errors are
            // not the only resync trigger: silently continuing here would
            // smear every following delta until the next periodic keyframe.
            fault()
            return
        }
        var block: CMBlockBuffer?
        guard CMBlockBufferCreateWithMemoryBlock(
            allocator: kCFAllocatorDefault, memoryBlock: nil, blockLength: avcc.count,
            blockAllocator: kCFAllocatorDefault, customBlockSource: nil,
            offsetToData: 0, dataLength: avcc.count, flags: 0, blockBufferOut: &block
        ) == kCMBlockBufferNoErr, let block else { return }
        let copy = avcc.withUnsafeBytes { raw -> OSStatus in
            guard let baseAddress = raw.baseAddress else { return -1 }
            return CMBlockBufferReplaceDataBytes(with: baseAddress, blockBuffer: block,
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

        let decodeGeneration = generationValue()
        incrementInflight()
        guard let session else {
            decrementInflight(generation: decodeGeneration)
            return
        }
        let status = VTDecompressionSessionDecodeFrame(
            session, sampleBuffer: sample,
            flags: [._EnableAsynchronousDecompression, ._1xRealTimePlayback],
            infoFlagsOut: nil
        ) { [weak self] status, _, buffer, _, _ in
            guard let self else { return }
            self.decrementInflight(generation: decodeGeneration)
            guard status == noErr else {
                log.error("decode callback status=\(status)")
                // Invalidating here would wait on this very callback.
                self.markCallbackFault()
                return
            }
            guard self.isCurrentGeneration(decodeGeneration), let buffer else { return }
            self.stateLock.withLock { self.decodedCount += 1 }
            self.onFrame?(buffer)
        }
        if status != noErr {
            decrementInflight(generation: decodeGeneration)
            fault()
        } else if takeCallbackFault() {
            fault()
        }
    }

    func invalidate() {
        if let session { VTDecompressionSessionInvalidate(session) }
        session = nil
        format = nil
        parameters = []
        stateLock.withLock {
            generation += 1
            inflight = 0
        }
    }

    private func fault() {
        stateLock.withLock {
            faultPending = false
            faultCount += 1
        }
        invalidate()
        onFault?()
    }

    private func takeCallbackFault() -> Bool {
        stateLock.withLock {
            let faulted = faultPending
            faultPending = false
            return faulted
        }
    }

    private func markCallbackFault() {
        stateLock.withLock { faultPending = true }
    }

    private func incrementInflight() {
        stateLock.withLock { inflight += 1 }
    }

    private func decrementInflight(generation: Int) {
        stateLock.withLock {
            guard self.generation == generation else { return }
            inflight -= 1
        }
    }

    private func inflightCount() -> Int {
        stateLock.withLock { inflight }
    }

    private func generationValue() -> Int {
        stateLock.withLock { generation }
    }

    private func isCurrentGeneration(_ generation: Int) -> Bool {
        stateLock.withLock { self.generation == generation }
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
            log.error("session create status=\(status)")
            return false
        }
        VTSessionSetProperty(session, key: kVTDecompressionPropertyKey_RealTime, value: kCFBooleanTrue)
        self.session = session
        return true
    }

    private static func parameterSetRank(_ set: Data, codec: Codec) -> Int {
        guard let first = set.first else { return Int.max }
        if codec == .h264 {
            switch first & 0x1f {
            case 7: return 0
            case 8: return 1
            default: return Int.max
            }
        }
        switch (first >> 1) & 0x3f {
        case 32: return 0
        case 33: return 1
        case 34: return 2
        default: return Int.max
        }
    }

    /// Order-independent comparison key for parameter sets: sorts by content
    /// so NAL ordering inside the access unit cannot masquerade as a change.
    private func sortedHex(_ sets: [Data]) -> [Data] {
        sets.sorted { $0.lexicographicallyPrecedes($1) }
    }

    /// Builds a format description from the codec's parameter sets.
    private static func format(codec: Codec, sets: [Data]) -> CMFormatDescription? {
        var format: CMFormatDescription?
        let ordered = sets.sorted { parameterSetRank($0, codec: codec) < parameterSetRank($1, codec: codec) }
        log.info("format: \(ordered.count) sets")
        let status = ordered.withParameterSets { pointers, sizes in
            guard let pointers, let sizes else { return OSStatus(-1) }
            switch codec {
            case .h264:
                guard ordered.count >= 2 else { return OSStatus(-1) }
                return CMVideoFormatDescriptionCreateFromH264ParameterSets(
                    allocator: kCFAllocatorDefault, parameterSetCount: ordered.count,
                    parameterSetPointers: pointers, parameterSetSizes: sizes,
                    nalUnitHeaderLength: 4, formatDescriptionOut: &format)
            case .av1:
                // AV1 never takes the Annex-B parameter-set path; VTAV1CoreDecoder
                // builds its format description from the sequence header OBU.
                return OSStatus(-1)
            case .hevc:
                guard ordered.count >= 3 else { return OSStatus(-1) }
                return CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                    allocator: kCFAllocatorDefault, parameterSetCount: ordered.count,
                    parameterSetPointers: pointers, parameterSetSizes: sizes,
                    nalUnitHeaderLength: 4, extensions: nil, formatDescriptionOut: &format)
            }
        }
        if status != noErr { log.error("format create status=\(status)") }
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
        // AV1 low-overhead OBUs tile the temporal unit exactly with explicit
        // sizes; Annex-B streams essentially never do. Check it first because
        // AV1 payloads may legitimately contain 00 00 01 byte patterns.
        let obus = AV1Bitstream.obus(au)
        if let last = obus.last, last.range.upperBound == au.count,
           obus.contains(where: { $0.type == 1 }),
           obus.contains(where: { $0.type == 3 || $0.type == 6 }) {
            return .av1
        }
        if hevc && !h264 { return .hevc }
        if h264 { return .h264 }
        return nil
    }

    /// Converts an Annex-B access unit to the 4-byte length-prefixed framing
    /// VTDecompressionSession consumes, collecting parameter-set NAL units.
    static func avcc(_ au: Data, codec: Codec, parameters: inout [Data]) -> Data? {
        guard codec != .av1 else { return nil }
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
        for nal in nals where !nal.isEmpty {
            let type = codec == .h264 ? nal[nal.startIndex] & 0x1f : (nal[nal.startIndex] >> 1) & 0x3f
            let parameterSet = codec == .h264 ? type == 7 || type == 8 : type == 32 || type == 33 || type == 34
            // Parameter sets belong in the format description, not in an AVCC sample.
            if parameterSet { continue }
            length = UInt32(nal.count).bigEndian
            withUnsafeBytes(of: &length) { out.append(contentsOf: $0) }
            out.append(nal)
        }
        return out.isEmpty ? nil : out
    }
}

private extension Array where Element == Data {
    /// Calls `body` with the parameter-set pointers and sizes. `NSData` keeps
    /// the backing storage alive and stable for the duration of the call.
    func withParameterSets<T>(
        _ body: (UnsafePointer<UnsafePointer<UInt8>>?, UnsafePointer<Int>?) -> T
    ) -> T {
        let storage = map { $0 as NSData }
        var pointers = storage.map { $0.bytes.assumingMemoryBound(to: UInt8.self) }
        var sizes = storage.map(\.count)
        return pointers.withUnsafeBufferPointer { p in
            sizes.withUnsafeBufferPointer { s in
                body(p.baseAddress, s.baseAddress)
            }
        }
    }
}
