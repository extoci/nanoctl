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

    func testTailnetMediaPathDetection() throws {
        XCTAssertTrue(try HostEndpoint("play.example.ts.net").usesTailnetMediaPath)
        XCTAssertTrue(try HostEndpoint("100.100.100.100").usesTailnetMediaPath)
        XCTAssertTrue(try HostEndpoint("100.64.0.1").usesTailnetMediaPath)
        XCTAssertTrue(try HostEndpoint("malachite").usesTailnetMediaPath)
        XCTAssertFalse(try HostEndpoint("100.63.255.255").usesTailnetMediaPath)
        XCTAssertFalse(try HostEndpoint("192.168.1.20").usesTailnetMediaPath)
        XCTAssertFalse(try HostEndpoint("desktop.local").usesTailnetMediaPath)
        XCTAssertFalse(try HostEndpoint("http://127.0.0.1:7777", allowSimulatorLoopback: true).usesTailnetMediaPath)
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

    func testGamepadBinaryMatchesAgentWireFormat() {
        // Same bytes as the agent's `Snapshot::parse_binary` test vector.
        let report = GamepadMessage(pads: [[2, 0x1001, 7, 8, -32768, 32767, -1, 1]], dualShock4: [
            .init(slot: 2, leftTriggerPressed: true, rightTriggerPressed: false, touchpadPressed: true)
        ])
        XCTAssertEqual(report.binary(sequence: 9), Data([
            2, 9, 4,
            0x01, 0x10, 7, 8, 0x00, 0x80, 0xff, 0x7f, 0xff, 0xff, 0x01, 0x00, 0x0b,
        ]))
        XCTAssertEqual(report.sparseBinary(sequence: 9), Data([
            12, 9, 4,
            0xff, 0x81, 0x20, 7, 8, 0x00, 0x80, 0xff, 0x7f, 0xff, 0xff, 0x01, 0x00, 0x0b,
        ]))
        XCTAssertEqual(GamepadMessage(pads: [[1, 0, 0, 7, -2, 0, 0, 0]]).sparseBinary(sequence: 10), Data([
            12, 10, 2, 12, 7, 0xfe, 0xff,
        ]))
        XCTAssertEqual(GamepadMessage(pads: []).binary(sequence: 0), Data([2, 0, 0]))
        XCTAssertNil(GamepadMessage(pads: [[0, 0x10000, 0, 0, 0, 0, 0, 0]]).binary(sequence: 0))
        XCTAssertNil(GamepadMessage(pads: Array(repeating: [0, 0, 0, 0, 0, 0, 0, 0], count: 5)).binary(sequence: 0))
    }

    func testHostDirectoryDecodesSelfAndPeers() throws {
        let directory = try JSONDecoder().decode(HostDirectory.self, from: Data(#"""
        {
          "self": {"hostName":"studio-mac","dnsName":"studio-mac.tail.ts.net","url":"https://studio-mac:7337"},
          "hosts": [
            {"hostName":"gaming-pc","dnsName":"gaming-pc.tail.ts.net","os":"windows","online":true,"url":"https://gaming-pc:7337"},
            {"hostName":"offline-linux","os":"linux","online":false,"url":"https://offline-linux:7337"}
          ]
        }
        """#.utf8))
        XCTAssertEqual(directory.selfHost.displayName, "studio-mac")
        XCTAssertEqual(directory.hosts.count, 2)
        XCTAssertTrue(directory.selfHost.online)
        XCTAssertNil(directory.selfHost.os)
        XCTAssertEqual(directory.hosts[0].os, "windows")
        XCTAssertNil(directory.hosts[1].dnsName)
    }

    func testGamepadStatusBatteryLabelHandlesUnknownAndCharging() {
        XCTAssertEqual(GamepadStatus(id: 1, slot: 0, name: "Controller", recognition: "DualSense",
                                     hostDevice: "DualShock 4", batteryPercent: 82,
                                     batteryState: "charging").batteryLabel, "82% · charging")
        XCTAssertEqual(GamepadStatus(id: 1, slot: 0, name: "Controller", recognition: "Xbox controller",
                                     hostDevice: "Xbox 360", batteryPercent: nil,
                                     batteryState: "").batteryLabel, "Battery unknown")
    }

    func testMultiGamepadBinaryUsesIndependentSlotsInSortedOrder() {
        let message = GamepadMessage(pads: [
            [2, 0x2000, 9, 0, 0, 0, 0, 0],
            [0, 0x1000, 7, 0, 100, -100, 0, 0],
        ], dualShock4: [
            .init(slot: 2, leftTriggerPressed: true, rightTriggerPressed: false, touchpadPressed: false),
        ])
        XCTAssertEqual(message.binary(sequence: 3), Data([
            2, 3, 5,
            0x00, 0x10, 7, 0, 100, 0, 0x9c, 0xff, 0, 0, 0, 0, 0,
            0x00, 0x20, 9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3,
        ]))
        XCTAssertEqual(message.sparseBinary(sequence: 3).map { Data($0.prefix(3)) }, Data([12, 3, 5]))
    }
}
