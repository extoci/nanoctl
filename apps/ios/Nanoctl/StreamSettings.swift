import Foundation

struct HostStreamSettings: Codable, Equatable {
    var codec: String
    var maxFps: Int?
    var maxBitrateKbps: Int?
}

/// Frame-rate choices worth exposing for a display. Sixty and thirty remain
/// useful even on ProMotion hardware; the display maximum is added exactly
/// once so a 120Hz device gets an explicit 120fps choice.
func streamFrameRateOptions(displayMaximum: Int) -> [Int] {
    Array(Set([30, 60, displayMaximum])).sorted(by: >)
}

enum StreamPreset: String, CaseIterable {
    case performance
    case balanced
    case quality
    case custom

    var label: String {
        switch self {
        case .performance: return "Performance"
        case .balanced: return "Balanced"
        case .quality: return "Quality"
        case .custom: return "Custom"
        }
    }

    /// Nil means the host maximum. Performance buys fluidity first, balanced
    /// saves bandwidth without dropping below 60fps, and quality removes both
    /// viewer-imposed ceilings.
    var caps: (maxFps: Int?, maxBitrateKbps: Int?)? {
        switch self {
        case .performance: return (nil, 20_000)
        case .balanced: return (60, 10_000)
        case .quality: return (nil, nil)
        case .custom: return nil
        }
    }

    static func infer(maxFps: Int?, maxBitrateKbps: Int?) -> StreamPreset {
        for preset in [StreamPreset.performance, .balanced, .quality] {
            if let caps = preset.caps,
               caps.maxFps == maxFps,
               caps.maxBitrateKbps == maxBitrateKbps {
                return preset
            }
        }
        return .custom
    }
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
