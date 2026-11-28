import XCTest
@testable import Nanoctl

@MainActor
final class KeyboardTests: XCTestCase {
    func testNamedKeysUseCompactWireIdentifiers() throws {
        let space = try XCTUnwrap(RemoteKey.special("Space"))
        XCTAssertEqual(space.binary(down: true, repeating: false), Data([5, 4, 6]))
        XCTAssertEqual(space.binary(down: false, repeating: false), Data([5, 5, 6]))
        let shift = try XCTUnwrap(RemoteKey.special("Shift"))
        XCTAssertEqual(shift.binary(down: true, repeating: false), Data([5, 4, 30]))
        let arrow = try XCTUnwrap(RemoteKey.special("ArrowRight"))
        XCTAssertEqual(arrow.binary(down: true, repeating: false), Data([5, 4, 10]))
    }

    func testPrintableKeysUseUtf8WireBytes() {
        let key = RemoteKey(code: "", value: "å")
        XCTAssertEqual(key.binary(down: true, repeating: false), Data([5, 0, 0xc3, 0xa5]))
        XCTAssertEqual(key.binary(down: true, repeating: true), Data([5, 2, 0xc3, 0xa5]))
        XCTAssertNil(RemoteKey(code: "", value: "too-long-for-the-thirty-two-byte-key-wire-limit")
            .binary(down: true, repeating: false))
    }

    func testStickyModifierWrapsFirstTextCharacterAndReleases() {
        var events: [(Bool, RemoteKey)] = []
        var texts: [String] = []
        let forwarder = KeyboardForwarder(sendKey: { down, key, _ in
            events.append((down, key))
            return true
        }, sendText: { text in
            texts.append(text)
            return true
        })
        let control = RemoteKey.special("Control")!
        XCTAssertTrue(forwarder.toggleSticky(control))
        XCTAssertTrue(forwarder.insertText("abc"))
        XCTAssertEqual(events.map { [$0.1.value, $0.0 ? "down" : "up"] }, [
            ["Control", "down"], ["a", "down"], ["a", "up"], ["Control", "up"],
        ])
        XCTAssertEqual(texts, ["bc"])
    }

    func testStickyShiftUppercasesFirstTextCharacterAndReleases() {
        var events: [(Bool, RemoteKey)] = []
        var texts: [String] = []
        let forwarder = KeyboardForwarder(sendKey: { down, key, _ in
            events.append((down, key))
            return true
        }, sendText: { text in
            texts.append(text)
            return true
        })
        let shift = RemoteKey.special("Shift")!
        XCTAssertTrue(forwarder.toggleSticky(shift))
        XCTAssertTrue(forwarder.insertText("abc"))
        XCTAssertEqual(events.map { [$0.1.value, $0.0 ? "down" : "up"] }, [
            ["Shift", "down"], ["Shift", "up"],
        ])
        XCTAssertEqual(texts, ["Abc"])
    }

    func testTapReleasesLatchedModifier() {
        var events: [(Bool, RemoteKey)] = []
        let forwarder = KeyboardForwarder(sendKey: { down, key, _ in
            events.append((down, key))
            return true
        }, sendText: { _ in true })
        let alt = RemoteKey.special("Alt")!
        let escape = RemoteKey.special("Escape")!
        forwarder.toggleSticky(alt)
        forwarder.tap(escape)
        XCTAssertEqual(events.map { [$0.1.value, $0.0 ? "down" : "up"] }, [
            ["Alt", "down"], ["Escape", "down"], ["Escape", "up"], ["Alt", "up"],
        ])
    }
}
