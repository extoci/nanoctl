import Foundation

struct HostStreamSettings: Codable, Equatable {
    var codec: String
    var maxFps: Int?
    var maxBitrateKbps: Int?
}

/// Per-host stream preferences. Hosts differ too much for one global cap to
/// be the right answer: a desktop on the same tailnet can use high bitrate
/// while a remote or thermally constrained machine needs a lower ceiling.
struct StreamSettingsStore {
    private static let storageKey = "hostStreamSettings"

    private let defaults: UserDefaults

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    func settings(for hostKey: String) -> HostStreamSettings? {
        all()[hostKey]
    }

    func save(_ settings: HostStreamSettings, for hostKey: String) {
        var stored = all()
        stored[hostKey] = settings
        guard let data = try? JSONEncoder().encode(stored) else { return }
        defaults.set(data, forKey: Self.storageKey)
    }

    private func all() -> [String: HostStreamSettings] {
        guard let data = defaults.data(forKey: Self.storageKey),
              let stored = try? JSONDecoder().decode([String: HostStreamSettings].self, from: data) else {
            return [:]
        }
        return stored
    }
}
