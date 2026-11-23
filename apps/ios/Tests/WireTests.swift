import CoreVideo
import WebRTC
import XCTest
@testable import Nanoctl

final class WireTests: XCTestCase {
    func testMailboxOnlyReturnsTheMostRecentFrame() throws {
        let mailbox = FrameMailbox()
        let previous = try XCTUnwrap(mailbox.latest(after: -1)).number
        var created: CVPixelBuffer?
        XCTAssertEqual(CVPixelBufferCreate(kCFAllocatorDefault, 2, 2, kCVPixelFormatType_32BGRA,
                                           nil, &created), kCVReturnSuccess)
        let buffer = RTCCVPixelBuffer(pixelBuffer: try XCTUnwrap(created))
        mailbox.renderFrame(RTCVideoFrame(buffer: buffer, rotation: ._0, timeStampNs: 1))
        mailbox.renderFrame(RTCVideoFrame(buffer: buffer, rotation: ._0, timeStampNs: 2))
        let latest = try XCTUnwrap(mailbox.latest(after: previous))
        XCTAssertEqual(latest.frame?.timeStampNs, 2)
        XCTAssertNil(mailbox.latest(after: latest.number))
        mailbox.renderFrame(nil)
        let cleared = try XCTUnwrap(mailbox.latest(after: latest.number))
        XCTAssertNil(cleared.frame)
    }

    #if DEBUG && targetEnvironment(simulator)
    func testMarkerDecodesFrameAndRejectsWrongChecksum() throws {
        let sequence = 487, input = 12
        let checksum = ((sequence >> 8) ^ sequence ^ input ^ 0xa5) & 255
        let word = (sequence << 16) | (input << 8) | checksum
        var created: CVPixelBuffer?
        XCTAssertEqual(CVPixelBufferCreate(kCFAllocatorDefault, 440, 4, kCVPixelFormatType_32BGRA,
                                           nil, &created), kCVReturnSuccess)
        let buffer = try XCTUnwrap(created)
        func paint(_ word: Int) throws {
            let bits = [1, 0] + (0..<32).reversed().map { (word >> $0) & 1 } + [0, 1]
            XCTAssertEqual(CVPixelBufferLockBaseAddress(buffer, []), kCVReturnSuccess)
            defer { CVPixelBufferUnlockBaseAddress(buffer, []) }
            let base = try XCTUnwrap(CVPixelBufferGetBaseAddress(buffer)).assumingMemoryBound(to: UInt8.self)
            for row in 0..<4 {
                for x in 0..<440 {
                    let offset = row * CVPixelBufferGetBytesPerRow(buffer) + x * 4
                    if x < 40 {
                        base[offset] = 255; base[offset + 1] = 255; base[offset + 2] = 0
                    } else if x >= 400 {
                        base[offset] = 255; base[offset + 1] = 0; base[offset + 2] = 255
                    } else {
                        let channel = UInt8(bits[(x - 40) / 10] * 255)
                        base[offset] = channel; base[offset + 1] = channel; base[offset + 2] = channel
                    }
                    base[offset + 3] = 255
                }
            }
        }
        try paint(word)
        XCTAssertEqual(FrameMarker.read(buffer)?.sequence, sequence)
        XCTAssertEqual(FrameMarker.read(buffer)?.input, input)
        try paint(word ^ 1)
        XCTAssertNil(FrameMarker.read(buffer))
    }
    #endif

    func testOriginIncludesPortAndDoesNotUseWebSocketScheme() throws {
        let endpoint = try HostEndpoint("play.example.ts.net")
        XCTAssertEqual(endpoint.origin, "https://play.example.ts.net:7337")
        XCTAssertEqual(endpoint.socket.absoluteString, "wss://play.example.ts.net:7337/v1/connect")
        let loopback = try HostEndpoint("http://127.0.0.1:7777", allowSimulatorLoopback: true)
        XCTAssertEqual(loopback.origin, "http://127.0.0.1:7777")
        XCTAssertEqual(loopback.socket.absoluteString, "ws://127.0.0.1:7777/v1/connect")
    }

    func testRejectsInsecureNonLoopbackAndAmbiguousURLs() {
        for url in ["http://127.0.0.1", "https://name:pass@host", "https://host/path", "https://host?x=1",
                    "https://host:0"] {
            XCTAssertThrowsError(try HostEndpoint(url))
        }
        XCTAssertThrowsError(try HostEndpoint("http://100.100.100.100:7337", allowSimulatorLoopback: true))
    }

    func testSignalVersionAndEpochIsolation() {
        let signal = SignalEnvelope(version: 2, sessionId: "s", controllerEpoch: "c", hostEpoch: "h",
                                    generation: 0, sequence: 0, sender: "host", sentAt: 1,
                                    payload: SignalPayload(type: "ice-complete"))
        XCTAssertTrue(signal.belongs(to: "s", controller: "c", generation: 0))
        XCTAssertFalse(signal.belongs(to: "new", controller: "c", generation: 0))
        XCTAssertFalse(signal.belongs(to: "s", controller: "new", generation: 0))
        XCTAssertFalse(signal.belongs(to: "s", controller: "c", generation: 1))
    }

    func testGamepadUsesHostCompatibleRangesAndOptionalExtension() throws {
        XCTAssertEqual(GamepadMessage.axis(-1), -32768)
        XCTAssertEqual(GamepadMessage.axis(1), 32767)
        XCTAssertEqual(GamepadMessage.axis(.nan), 0)
        XCTAssertEqual(GamepadMessage.trigger(0.5), 128)
        let pads = [[0, 0x1000, 255, 0, 0, -32768, 32767, 0]]
        func encode(_ message: GamepadMessage) throws -> [String: Any] {
            let data = try JSONEncoder().encode(message)
            return try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        }
        let standard = try encode(.init(pads: pads))
        XCTAssertEqual(standard["type"] as? String, "gamepad")
        XCTAssertEqual(standard["pads"] as? [[Int]], pads)
        XCTAssertNil(standard["dualShock4"])
        let sony = try encode(.init(pads: pads, dualShock4: [
            .init(slot: 0, leftTriggerPressed: true, rightTriggerPressed: false, touchpadPressed: false)
        ]))
        let extensionData = try XCTUnwrap((sony["dualShock4"] as? [[String: Any]])?.first)
        XCTAssertEqual(extensionData["slot"] as? Int, 0)
        XCTAssertEqual(extensionData["leftTriggerPressed"] as? Bool, true)
    }
}
