import XCTest
@testable import Nanoctl

final class StreamDiagnosticsTests: XCTestCase {
    func testOverlaySeparatesVideoNetworkAndDecoderFacts() {
        let diagnostics = StreamDiagnostics(
            transport: "DC",
            codec: "hevc",
            resolution: "2560×1440",
            fps: 60,
            bitrateKbps: 24_500,
            rttMs: 8,
            path: "tailnet vpn direct host→host",
            lossPercent: 0.5,
            decoder: "VideoToolbox",
            dropped: 2
        )
        XCTAssertEqual(diagnostics.overlayText, """
        DC · hevc · 2560×1440 · 60fps
        24.5 Mbps · 8ms RTT · tailnet vpn direct host→host · loss 0.5%
        VideoToolbox · drop 2
        """)
    }

    func testPathLabelsRelayAndOmitsUnknownNetworkType() {
        XCTAssertEqual(
            StreamDiagnostics.pathLabel(localType: "relay", remoteType: "host", networkType: "unknown", tailnet: true),
            "tailnet relay relay→host"
        )
        XCTAssertEqual(
            StreamDiagnostics.pathLabel(localType: "host", remoteType: "host", networkType: "vpn", tailnet: false),
            "vpn direct host→host"
        )
        XCTAssertNil(StreamDiagnostics.pathLabel(localType: nil, remoteType: nil, networkType: nil, tailnet: false))
    }

    func testRttLabelPreservesSubMillisecondEvidence() {
        XCTAssertEqual(StreamDiagnostics.rttLabel(ms: 0.4), "<1ms RTT")
        XCTAssertEqual(StreamDiagnostics.rttLabel(ms: 8.4), "8ms RTT")
    }

    func testBitrateLabelUsesMbpsAboveOneMegabit() {
        XCTAssertEqual(StreamDiagnostics.bitrateLabel(kbps: 999), "999 kbps")
        XCTAssertEqual(StreamDiagnostics.bitrateLabel(kbps: 1000), "1.0 Mbps")
    }
}
