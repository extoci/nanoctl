import CoreMedia
import CoreVideo
import Foundation
import OSLog
import VideoToolbox
import WebRTC

private let av1Log = Logger(subsystem: "ai.nanoctl.viewer", category: "vtav1")

/// Low-overhead-OBU AV1 decoder backed by VideoToolbox. The same core serves
/// the jitter-buffered RTP decoder and nanoctl's direct data-channel path.
final class VTAV1CoreDecoder {
    /// Decoded image and the presentation timestamp supplied to `decode`.
    var onFrame: ((CVPixelBuffer, CMTime) -> Void)?
    /// Decoder or bitstream failure. The caller should resynchronize from a keyframe.
    var onFault: (() -> Void)?

    private(set) var decoded = 0
    private(set) var dropped = 0
    private(set) var faults = 0

    private var session: VTDecompressionSession?
    private var format: CMFormatDescription?
    private let stateLock = NSLock()
    private var callbackFaulted = false
    private var inflight = 0
    private var generation = 0
    private let maxInflight: Int?

    init(maxInflight: Int? = nil) { self.maxInflight = maxInflight }

    deinit { invalidate() }

    /// Feeds one temporal unit in the AV1 low-overhead bitstream format.
    /// Returns false when the frame could not be accepted; callback failures
    /// are reported here on the next frame because VideoToolbox cannot be
    /// invalidated from inside its own callback.
    @discardableResult
    func decode(_ data: Data, timestamp: Int64, timescale: CMTimeScale, isKey: Bool = false) -> Bool {
        if takeCallbackFault() {
            return fault()
        }
        guard !data.isEmpty else { return false }
        if let discovered = AV1Bitstream.formatDescription(data),
           !(format.map({ CMFormatDescriptionEqual($0, otherFormatDescription: discovered) }) ?? false) {
            invalidate()
            format = discovered
        }
        if session == nil {
            guard let format else { return false }
            guard createSession(format: format) else { return fault() }
        }
        if let maxInflight, inflightCount() > maxInflight, !isKey {
            dropped += 1
            return true
        }
        guard let session, let format else { return false }

        var block: CMBlockBuffer?
        guard CMBlockBufferCreateWithMemoryBlock(
            allocator: kCFAllocatorDefault,
            memoryBlock: nil,
            blockLength: data.count,
            blockAllocator: kCFAllocatorDefault,
            customBlockSource: nil,
            offsetToData: 0,
            dataLength: data.count,
            flags: 0,
            blockBufferOut: &block
        ) == kCMBlockBufferNoErr, let block else { return fault() }
        let copied = data.withUnsafeBytes { raw -> OSStatus in
            guard let base = raw.baseAddress else { return -1 }
            return CMBlockBufferReplaceDataBytes(
                with: base,
                blockBuffer: block,
                offsetIntoDestination: 0,
                dataLength: data.count
            )
        }
        guard copied == kCMBlockBufferNoErr else { return fault() }

        var timing = CMSampleTimingInfo(
            duration: CMTime.invalid,
            presentationTimeStamp: CMTime(value: timestamp, timescale: timescale),
            decodeTimeStamp: CMTime.invalid
        )
        var sizes = [data.count]
        var sample: CMSampleBuffer?
        guard CMSampleBufferCreate(
            allocator: kCFAllocatorDefault,
            dataBuffer: block,
            dataReady: true,
            makeDataReadyCallback: nil,
            refcon: nil,
            formatDescription: format,
            sampleCount: 1,
            sampleTimingEntryCount: 1,
            sampleTimingArray: &timing,
            sampleSizeEntryCount: 1,
            sampleSizeArray: &sizes,
            sampleBufferOut: &sample
        ) == noErr, let sample else { return fault() }
        if let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: true),
           CFArrayGetCount(attachments) > 0 {
            let first = unsafeBitCast(CFArrayGetValueAtIndex(attachments, 0), to: CFMutableDictionary.self)
            CFDictionarySetValue(
                first,
                Unmanaged.passUnretained(kCMSampleAttachmentKey_DisplayImmediately).toOpaque(),
                Unmanaged.passUnretained(kCFBooleanTrue).toOpaque()
            )
        }

        let decodeGeneration = generationValue()
        incrementInflight()
        let status = VTDecompressionSessionDecodeFrame(
            session,
            sampleBuffer: sample,
            flags: [._EnableAsynchronousDecompression, ._1xRealTimePlayback],
            infoFlagsOut: nil
        ) { [weak self] status, _, imageBuffer, presentation, _ in
            guard let self else { return }
            self.decrementInflight(generation: decodeGeneration)
            guard status == noErr else {
                av1Log.error("AV1 decode callback status=\(status, privacy: .public)")
                self.markCallbackFault()
                return
            }
            guard self.isCurrentGeneration(decodeGeneration), let imageBuffer else { return }
            self.decoded += 1
            self.onFrame?(imageBuffer, presentation)
        }
        if status != noErr {
            decrementInflight(generation: decodeGeneration)
            av1Log.error("AV1 decode submit status=\(status, privacy: .public)")
            return fault()
        }
        return true
    }

    func invalidate() {
        if let session { VTDecompressionSessionInvalidate(session) }
        session = nil
        format = nil
        stateLock.withLock {
            generation += 1
            inflight = 0
        }
    }

    private func fault() -> Bool {
        faults += 1
        invalidate()
        onFault?()
        return false
    }

    private func createSession(format: CMFormatDescription) -> Bool {
        var session: VTDecompressionSession?
        let attributes: [CFString: Any] = [kCVPixelBufferMetalCompatibilityKey: true]
        let status = VTDecompressionSessionCreate(
            allocator: kCFAllocatorDefault,
            formatDescription: format,
            decoderSpecification: nil,
            imageBufferAttributes: attributes as CFDictionary,
            outputCallback: nil,
            decompressionSessionOut: &session
        )
        guard status == noErr, let session else {
            av1Log.error("AV1 session create status=\(status, privacy: .public)")
            return false
        }
        VTSessionSetProperty(session, key: kVTDecompressionPropertyKey_RealTime, value: kCFBooleanTrue)
        self.session = session
        return true
    }

    private func takeCallbackFault() -> Bool {
        stateLock.withLock {
            let faulted = callbackFaulted
            callbackFaulted = false
            return faulted
        }
    }

    private func markCallbackFault() {
        stateLock.withLock { callbackFaulted = true }
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
}

/// VideoToolbox AV1 decoder plugged into libwebrtc's RTP decode pipeline.
/// stasel's bundled AV1 decoder is dav1d (software); on devices with an AV1
/// decoder block this keeps AV1 viable without paying the CPU and latency cost
/// of software 4K decode.
final class VTAV1Decoder: NSObject, RTCVideoDecoder {
    private let core = VTAV1CoreDecoder()
    private var callback: RTCVideoDecoderCallback?

    override init() {
        super.init()
        core.onFrame = { [weak self] imageBuffer, presentation in
            guard let self else { return }
            let frameBuffer = RTCCVPixelBuffer(pixelBuffer: imageBuffer)
            let seconds = CMTimeGetSeconds(presentation)
            let frame = RTCVideoFrame(
                buffer: frameBuffer,
                rotation: ._0,
                timeStampNs: Int64(seconds * 1_000_000_000)
            )
            frame.timeStamp = Int32(bitPattern: UInt32(truncatingIfNeeded: presentation.value))
            self.callback?(frame)
        }
    }

    func setCallback(_ callback: @escaping RTCVideoDecoderCallback) {
        self.callback = callback
    }

    func startDecode(withNumberOfCores numberOfCores: Int32) -> Int { 0 }

    func release() -> Int {
        core.invalidate()
        return 0
    }

    func implementationName() -> String { "VideoToolbox-AV1" }

    func decode(
        _ image: RTCEncodedImage,
        missingFrames: Bool,
        codecSpecificInfo info: RTCCodecSpecificInfo?,
        renderTimeMs: Int64
    ) -> Int {
        core.decode(
            image.buffer,
            timestamp: Int64(image.timeStamp),
            timescale: 90_000,
            isKey: image.frameType == .videoFrameKey
        ) ? 0 : -1
    }
}

/// Uses VideoToolbox for AV1 where the device has a hardware decoder and the
/// bundled WebRTC factory for every other codec/fallback case.
final class NanoctlDecoderFactory: NSObject, RTCVideoDecoderFactory {
    private let fallback = RTCDefaultVideoDecoderFactory()

    func supportedCodecs() -> [RTCVideoCodecInfo] { fallback.supportedCodecs() }

    func createDecoder(_ info: RTCVideoCodecInfo) -> RTCVideoDecoder? {
        if info.name.lowercased() == "av1" {
            if #available(iOS 26.2, *) {
                VTRegisterSupplementalVideoDecoderIfAvailable(kCMVideoCodecType_AV1)
            }
        }
        if info.name.lowercased() == "av1", VTIsHardwareDecodeSupported(kCMVideoCodecType_AV1) {
            return VTAV1Decoder()
        }
        return fallback.createDecoder(info)
    }
}
