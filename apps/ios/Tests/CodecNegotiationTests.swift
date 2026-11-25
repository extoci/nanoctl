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
