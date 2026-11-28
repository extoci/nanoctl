import XCTest
@testable import Nanoctl

final class ConnectProgressTests: XCTestCase {
    func testProgressAdvancesWithoutRegressingAndClearsFailure() {
        var progress = ConnectProgress()
        progress.advance(to: .networking, detail: "Checking network paths…")
        progress.fail("Connection lost.")
        XCTAssertEqual(progress.stage, .networking)
        XCTAssertEqual(progress.failure, "Connection lost.")
        XCTAssertNotNil(progress.failureHint)

        progress.advance(to: .signaling, detail: "Stale")
        XCTAssertEqual(progress.detail, "Connection lost.")
        progress.advance(to: .media, detail: "Waiting for video…")
        XCTAssertEqual(progress.stage, .media)
        XCTAssertEqual(progress.detail, "Waiting for video…")
        XCTAssertNil(progress.failure)
        XCTAssertNil(progress.failureHint)
    }

    func testFailureHintsAreActionable() {
        XCTAssertEqual(
            ConnectProgress.hint(for: "The host did not establish a connection."),
            "Check that the host is online, nanoctl is running, and Tailscale is connected."
        )
        XCTAssertEqual(
            ConnectProgress.hint(for: "This viewer requires agent protocol 2."),
            "Update nanoctl on both this device and the host."
        )
        XCTAssertEqual(
            ConnectProgress.hint(for: "Could not connect to the server."),
            "Check that the host is online and nanoctl is running, then try again."
        )
        XCTAssertTrue(ConnectProgress.hint(for: "Connection lost.").contains("network path"))
    }
}
