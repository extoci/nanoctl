import XCTest
@testable import Nanoctl

final class StreamSettingsTests: XCTestCase {
    private func makeDefaults() throws -> (UserDefaults, String) {
        let suite = "StreamSettingsTests.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        return (defaults, suite)
    }

    func testRoundTripsSettingsIndependentlyPerHost() throws {
        let (defaults, suite) = try makeDefaults()
        defer { defaults.removePersistentDomain(forName: suite) }
        let store = StreamSettingsStore(defaults: defaults)
        let desktop = "https://desktop.example.ts.net:7337"
        let laptop = "https://laptop.example.ts.net:7337"

        store.save(HostStreamSettings(codec: "hevc", maxFps: 120, maxBitrateKbps: 80_000), for: desktop)
        store.save(HostStreamSettings(codec: "h264", maxFps: 30, maxBitrateKbps: 5_000), for: laptop)

        XCTAssertEqual(store.settings(for: desktop),
                       HostStreamSettings(codec: "hevc", maxFps: 120, maxBitrateKbps: 80_000))
        XCTAssertEqual(store.settings(for: laptop),
                       HostStreamSettings(codec: "h264", maxFps: 30, maxBitrateKbps: 5_000))
    }

    func testUnlimitedValuesAndUnknownHostRemainDistinct() throws {
        let (defaults, suite) = try makeDefaults()
        defer { defaults.removePersistentDomain(forName: suite) }
        let store = StreamSettingsStore(defaults: defaults)
        let host = "https://desktop.example.ts.net:7337"

        XCTAssertNil(store.settings(for: host))
        store.save(HostStreamSettings(codec: "auto", maxFps: nil, maxBitrateKbps: nil), for: host)

        XCTAssertEqual(store.settings(for: host), HostStreamSettings(codec: "auto", maxFps: nil, maxBitrateKbps: nil))
        XCTAssertNil(store.settings(for: "https://other.example.ts.net:7337"))
    }
}
