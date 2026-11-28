import CoreMedia
import VideoToolbox
import WebRTC
import XCTest
@testable import Nanoctl

final class CodecNegotiationTests: XCTestCase {
    func testAutoPrefersHardwareDecodeByEfficiencyAndFallsBackToH264() {
        let order = CodecPreference.orderedCodecs(
            preference: .auto,
            hostCodecs: ["av1", "hevc", "h264"],
            receivableCodecs: ["av1", "h264"]
        ) { codec in codec == "av1" }
        XCTAssertEqual(order, ["av1", "h264", "hevc"])
    }

    func testAutoKeepsEfficiencyOrderWhenAdvancedCodecsAreHardwareDecodable() {
        let order = CodecPreference.orderedCodecs(
            preference: .auto,
            hostCodecs: ["av1", "hevc", "h264"],
            receivableCodecs: ["av1", "h264"]
        ) { codec in codec == "av1" || codec == "hevc" }
        XCTAssertEqual(order, ["av1", "hevc", "h264"])
    }

    func testAutoDoesNotOfferCodecHostCannotEncode() {
        let order = CodecPreference.orderedCodecs(
            preference: .auto,
            hostCodecs: ["hevc", "h264"],
            receivableCodecs: ["av1", "h264"]
        ) { _ in true }
        XCTAssertEqual(order, ["hevc", "h264"])
    }

    func testExplicitChoiceLeadsWhenBothSidesSupportIt() {
        let order = CodecPreference.orderedCodecs(
            preference: .av1,
            hostCodecs: ["av1", "hevc", "h264"],
            receivableCodecs: ["av1", "h264"]
        ) { _ in false }
        XCTAssertEqual(order, ["av1", "hevc", "h264"])
    }

    func testHevcInjectionFollowsAV1AndPrecedesH264() throws {
        let sdp = [
            "v=0",
            "m=video 9 UDP/TLS/RTP/SAVPF 96 97 98 99",
            "a=rtpmap:96 H264/90000",
            "a=rtpmap:97 rtx/90000",
            "a=rtpmap:98 AV1/90000",
            "a=rtpmap:99 rtx/90000",
        ].joined(separator: "\r\n")
        let rewritten = try XCTUnwrap(SDP.injectHevc(sdp, order: ["av1", "hevc", "h264"]))
        let video = try XCTUnwrap(rewritten.components(separatedBy: "\r\n").first { $0.hasPrefix("m=video ") })
        XCTAssertTrue(video.hasPrefix("m=video 9 UDP/TLS/RTP/SAVPF 98 35 96"), video)
        XCTAssertTrue(rewritten.contains("a=rtpmap:35 H265/90000"))
    }
}

final class AV1BitstreamTests: XCTestCase {
    func testBuildsFormatDescriptionFromLowOverheadSequenceHeader() throws {
        let bytes: [UInt8] = [
            0x12, 0x00, // temporal delimiter
            0x0a, 0x0b, // sequence header, 11-byte payload
            0x02, 0x00, 0x00, 0x05, 0x5d, 0xa7, 0xfb, 0x8d, 0x5f, 0x30, 0x08,
        ]
        let format = try XCTUnwrap(AV1Bitstream.formatDescription(Data(bytes)))
        XCTAssertEqual(CMVideoFormatDescriptionGetDimensions(format).width, 160)
        XCTAssertEqual(CMVideoFormatDescriptionGetDimensions(format).height, 120)
    }
}

final class NanoctlDecoderFactoryTests: XCTestCase {
    func testDetectCodecSniffsAV1TemporalUnit() {
        // Temporal delimiter + sequence header + frame OBU, exactly tiled,
        // no Annex-B start codes.
        let bytes: [UInt8] = [
            0x12, 0x00,
            0x0a, 0x0b,
            0x02, 0x00, 0x00, 0x05, 0x5d, 0xa7, 0xfb, 0x8d, 0x5f, 0x30, 0x08,
            0x32, 0x01, 0x00,
        ]
        XCTAssertEqual(VTDecoder.detectCodec(Data(bytes)), .av1)
    }

    func testAV1UsesVideoToolboxOnlyWhenHardwareDecodeExists() {
        let decoder = NanoctlDecoderFactory().createDecoder(RTCVideoCodecInfo(name: "AV1"))
        if #available(iOS 26.2, *) {
            VTRegisterSupplementalVideoDecoderIfAvailable(kCMVideoCodecType_AV1)
        }
        if VTAV1CoreDecoder.videoToolboxAvailable {
            XCTAssertEqual(decoder?.implementationName(), "VideoToolbox-AV1")
        } else {
            XCTAssertNotEqual(decoder?.implementationName(), "VideoToolbox-AV1")
        }
    }
}
