import CoreMedia
import CoreVideo
import Foundation
import VideoToolbox

@main
struct AV1VideoToolboxProof {
    static func main() throws {
    let path = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "/tmp/av1-test.ivf"
    let bytes = try Data(contentsOf: URL(fileURLWithPath: path))
    guard bytes.count > 32, bytes[0] == 0x44, bytes[1] == 0x4b, bytes[2] == 0x49, bytes[3] == 0x46 else {
        fatalError("not an IVF stream")
    }

    var frames: [Data] = []
    var offset = 32
    while offset + 12 <= bytes.count {
        let size = Int(bytes[offset]) | Int(bytes[offset + 1]) << 8 | Int(bytes[offset + 2]) << 16 | Int(bytes[offset + 3]) << 24
        let start = offset + 12
        guard start + size <= bytes.count else { break }
        frames.append(bytes.subdata(in: start..<start + size))
        offset = start + size
    }
    guard let first = frames.first, let format = AV1Bitstream.formatDescription(first) else {
        fatalError("fixture did not produce an AV1 format description")
    }

    var session: VTDecompressionSession?
    let attributes: [CFString: Any] = [kCVPixelBufferMetalCompatibilityKey: true]
    let createStatus = VTDecompressionSessionCreate(
        allocator: kCFAllocatorDefault,
        formatDescription: format,
        decoderSpecification: nil,
        imageBufferAttributes: attributes as CFDictionary,
        outputCallback: nil,
        decompressionSessionOut: &session
    )
    guard createStatus == noErr, let session else {
        fatalError("VideoToolbox session create status \(createStatus)")
    }

    var decoded = 0
    var failures: [OSStatus] = []
    let lock = NSLock()
    for (index, data) in frames.prefix(30).enumerated() {
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
        ) == kCMBlockBufferNoErr, let block else { fatalError("CMBlockBuffer creation failed") }
        let copyStatus = data.withUnsafeBytes { raw -> OSStatus in
            guard let base = raw.baseAddress else { return -1 }
            return CMBlockBufferReplaceDataBytes(
                with: base,
                blockBuffer: block,
                offsetIntoDestination: 0,
                dataLength: data.count
            )
        }
        guard copyStatus == kCMBlockBufferNoErr else { fatalError("CMBlockBuffer copy status \(copyStatus)") }

        var timing = CMSampleTimingInfo(
            duration: CMTime.invalid,
            presentationTimeStamp: CMTime(value: Int64(index * 3_000), timescale: 90_000),
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
        ) == noErr, let sample else { fatalError("CMSampleBuffer creation failed") }
        if let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: true),
           CFArrayGetCount(attachments) > 0 {
            let firstAttachment = unsafeBitCast(CFArrayGetValueAtIndex(attachments, 0), to: CFMutableDictionary.self)
            CFDictionarySetValue(
                firstAttachment,
                Unmanaged.passUnretained(kCMSampleAttachmentKey_DisplayImmediately).toOpaque(),
                Unmanaged.passUnretained(kCFBooleanTrue).toOpaque()
            )
        }

        let submitStatus = VTDecompressionSessionDecodeFrame(
            session,
            sampleBuffer: sample,
            flags: [._EnableAsynchronousDecompression, ._1xRealTimePlayback],
            infoFlagsOut: nil
        ) { status, _, image, _, _ in
            lock.lock()
            if status != noErr {
                failures.append(status)
            } else if image != nil {
                decoded += 1
            }
            lock.unlock()
        }
        if submitStatus != noErr { failures.append(submitStatus) }
    }
    VTDecompressionSessionWaitForAsynchronousFrames(session)
    VTDecompressionSessionInvalidate(session)

    print("frames=\(min(30, frames.count)) decoded=\(decoded) failures=\(failures)")
    if decoded == 0 { exit(1) }
    }
}
