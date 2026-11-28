import CoreMedia
import Foundation

/// Minimal AV1 low-overhead bitstream parser used to build the `av1C`
/// configuration record VideoToolbox expects. AV1 sequence-header parsing is
/// inherently bit-level; the field walk follows the AV1 specification and the
/// permissively reusable implementation in ALVR's AV1Parser.swift.
enum AV1Bitstream {
    struct SequenceInfo: Equatable {
        var profile = 0
        var level = 0
        var tier = 0
        var highBitdepth = 0
        var twelveBit = 0
        var monochrome = 0
        var subsamplingX = 1
        var subsamplingY = 1
        var chromaSamplePosition = 0
        var initialPresentationDelayPresent = 0
        var initialPresentationDelay = 0
        var width = 0
        var height = 0
        var bitdepth = 8
    }

    struct OBU {
        let type: UInt8
        let range: Range<Int>
        let payload: Range<Int>
        let hasSize: Bool
    }

    static func obus(_ data: Data) -> [OBU] {
        var result: [OBU] = []
        var offset = 0
        while offset < data.count {
            let start = offset
            let header = data[offset]
            guard header & 0x80 == 0 else { return result }
            let type = (header >> 3) & 0x0f
            let hasExtension = header & 0x04 != 0
            let hasSize = header & 0x02 != 0
            offset += 1
            if hasExtension {
                guard offset < data.count else { return result }
                offset += 1
            }
            let size: Int
            if hasSize {
                guard let read = leb128(data, at: offset) else { return result }
                size = read.value
                offset += read.length
            } else {
                size = data.count - offset
            }
            guard size >= 0, offset + size <= data.count else { return result }
            let end = offset + size
            result.append(OBU(type: type, range: start..<end, payload: offset..<end, hasSize: hasSize))
            offset = end
        }
        return result
    }

    static func formatDescription(_ temporalUnit: Data) -> CMFormatDescription? {
        guard let sequence = obus(temporalUnit).first(where: { $0.type == 1 }),
              let info = parseSequenceHeader(temporalUnit.subdata(in: sequence.payload)) else { return nil }
        let config: Data
        if sequence.hasSize {
            config = temporalUnit.subdata(in: sequence.range)
        } else {
            var rebuilt = Data([0x0a])
            appendLeb128(sequence.payload.count, to: &rebuilt)
            rebuilt.append(temporalUnit.subdata(in: sequence.payload))
            config = rebuilt
        }
        var record = Data([0x81])
        record.append(UInt8((info.profile & 7) << 5) | UInt8(info.level & 0x1f))
        let flags = ((info.tier & 1) << 7)
            | ((info.highBitdepth & 1) << 6)
            | ((info.twelveBit & 1) << 5)
            | ((info.monochrome & 1) << 4)
            | ((info.subsamplingX & 1) << 3)
            | ((info.subsamplingY & 1) << 2)
            | (info.chromaSamplePosition & 3)
        record.append(UInt8(flags))
        record.append(UInt8((info.initialPresentationDelayPresent & 1) << 4)
            | UInt8(info.initialPresentationDelayPresent == 1 ? info.initialPresentationDelay & 0x0f : 0))
        record.append(config)

        let extensions: [CFString: Any] = [
            kCMFormatDescriptionExtension_BitsPerComponent: info.bitdepth,
            kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms: ["av1C": record],
        ]
        var format: CMFormatDescription?
        let status = CMVideoFormatDescriptionCreate(
            allocator: kCFAllocatorDefault,
            codecType: kCMVideoCodecType_AV1,
            width: Int32(info.width),
            height: Int32(info.height),
            extensions: extensions as CFDictionary,
            formatDescriptionOut: &format
        )
        return status == noErr ? format : nil
    }

    private static func leb128(_ data: Data, at start: Int) -> (value: Int, length: Int)? {
        var value = 0
        var shift = 0
        var index = start
        while index < data.count {
            let byte = data[index]
            value |= Int(byte & 0x7f) << shift
            index += 1
            if byte & 0x80 == 0 { return (value, index - start) }
            shift += 7
            guard shift < 32 else { return nil }
        }
        return nil
    }

    private static func appendLeb128(_ value: Int, to data: inout Data) {
        var value = value
        while true {
            var byte = UInt8(value & 0x7f)
            value >>= 7
            if value != 0 { byte |= 0x80 }
            data.append(byte)
            if value == 0 { return }
        }
    }

    private struct Bits {
        let data: Data
        var offset = 0

        mutating func bit() -> Int {
            guard offset / 8 < data.count else { return 0 }
            let value = Int((data[offset / 8] >> UInt8(7 - offset % 8)) & 1)
            offset += 1
            return value
        }

        mutating func bits(_ count: Int) -> Int {
            var value = 0
            for _ in 0..<count { value = (value << 1) | bit() }
            return value
        }

        mutating func uvlc() -> Int {
            var zeros = 0
            while zeros < 32 && bit() == 0 { zeros += 1 }
            guard zeros < 32 else { return -1 }
            var suffix = 0
            for _ in 0..<zeros { suffix = (suffix << 1) | bit() }
            return (1 << zeros) - 1 + suffix
        }
    }

    private static func parseSequenceHeader(_ payload: Data) -> SequenceInfo? {
        var bits = Bits(data: payload)
        var info = SequenceInfo()
        info.profile = bits.bits(3)
        _ = bits.bit() // still_picture
        let reducedStillPicture = bits.bit() == 1
        var decoderModelInfoPresent = false
        var initialDelayByOperatingPoint: [Int] = []
        if reducedStillPicture {
            info.level = bits.bits(5)
        } else {
            if bits.bit() == 1 {
                _ = bits.bits(32)
                _ = bits.bits(32)
                if bits.bit() == 1 { _ = bits.uvlc() }
                decoderModelInfoPresent = bits.bit() == 1
                if decoderModelInfoPresent {
                    _ = bits.bits(5)
                    _ = bits.bits(32)
                    _ = bits.bits(5)
                    _ = bits.bits(5)
                }
            }
            info.initialPresentationDelayPresent = bits.bit()
            let operatingPoints = bits.bits(5) + 1
            for index in 0..<operatingPoints {
                _ = bits.bits(12)
                let level = bits.bits(5)
                var tier = 0
                if level > 7 { tier = bits.bit() }
                if index == 0 {
                    info.level = level
                    info.tier = tier
                }
                if decoderModelInfoPresent, bits.bit() == 1 {
                    // nanoctl's encoders do not use the decoder model. Refuse
                    // to guess its variable-length operating parameters.
                    return nil
                }
                if info.initialPresentationDelayPresent == 1 {
                    if bits.bit() == 1 {
                        initialDelayByOperatingPoint.append(bits.bits(4))
                    } else {
                        initialDelayByOperatingPoint.append(0)
                    }
                }
            }
            info.initialPresentationDelay = initialDelayByOperatingPoint.first ?? 0
        }
        let widthBits = bits.bits(4) + 1
        let heightBits = bits.bits(4) + 1
        info.width = bits.bits(widthBits) + 1
        info.height = bits.bits(heightBits) + 1
        guard info.width > 0, info.height > 0 else { return nil }

        if !reducedStillPicture, bits.bit() == 1 {
            _ = bits.bits(4)
            _ = bits.bits(3)
        }
        _ = bits.bit() // use_128x128_superblock
        _ = bits.bit() // enable_filter_intra
        _ = bits.bit() // enable_intra_edge_filter
        if !reducedStillPicture {
            _ = bits.bit() // enable_interintra_compound
            _ = bits.bit() // enable_masked_compound
            _ = bits.bit() // enable_warped_motion
            _ = bits.bit() // enable_dual_filter
            let orderHint = bits.bit()
            if orderHint == 1 {
                _ = bits.bit() // enable_jnt_comp
                _ = bits.bit() // enable_ref_frame_mvs
            }
            let chooseScreenContent = bits.bit()
            let screenContentTools = chooseScreenContent == 1 ? 2 : bits.bit()
            if screenContentTools > 0, bits.bit() == 0 { _ = bits.bit() }
            if orderHint == 1 { _ = bits.bits(3) }
        }
        _ = bits.bit() // enable_superres
        _ = bits.bit() // enable_cdef
        _ = bits.bit() // enable_restoration

        info.highBitdepth = bits.bit()
        if info.profile == 2, info.highBitdepth == 1 {
            info.twelveBit = bits.bit()
            info.bitdepth = info.twelveBit == 1 ? 12 : 10
        } else {
            info.bitdepth = info.highBitdepth == 1 ? 10 : 8
        }
        if info.profile != 1 { info.monochrome = bits.bit() }
        let colorDescriptionPresent = bits.bit()
        var primaries = 2, transfer = 2, matrix = 2
        if colorDescriptionPresent == 1 {
            primaries = bits.bits(8)
            transfer = bits.bits(8)
            matrix = bits.bits(8)
        }
        if info.monochrome == 1 {
            _ = bits.bit()
        } else if primaries == 1, transfer == 13, matrix == 0 {
            info.subsamplingX = 0
            info.subsamplingY = 0
            _ = bits.bit()
        } else {
            _ = bits.bit() // color_range
            if info.profile == 0 {
                info.subsamplingX = 1
                info.subsamplingY = 1
            } else if info.profile == 1 {
                info.subsamplingX = 0
                info.subsamplingY = 0
            } else if info.bitdepth == 12 {
                info.subsamplingX = bits.bit()
                info.subsamplingY = info.subsamplingX == 1 ? bits.bit() : 0
            } else {
                info.subsamplingX = 1
                info.subsamplingY = 0
            }
            if info.subsamplingX == 1, info.subsamplingY == 1 {
                info.chromaSamplePosition = bits.bits(2)
            }
            _ = bits.bit() // separate_uv_delta_q
        }
        return info
    }
}
