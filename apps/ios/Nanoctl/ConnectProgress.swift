import Foundation

enum ConnectStage: Int, CaseIterable {
    case signaling = 0
    case networking = 1
    case media = 2
    case live = 3

    var label: String {
        switch self {
        case .signaling: return "Signaling"
        case .networking: return "Networking"
        case .media: return "Media"
        case .live: return "Live"
        }
    }
}

/// User-facing connection progress. Unlike the historical free-form status,
/// the failing stage is preserved while a retry is scheduled so the UI can
/// explain what broke instead of restarting a vague spinner.
struct ConnectProgress: Equatable {
    var stage = ConnectStage.signaling
    var detail = "Contacting host…"
    var failure: String?
    var failureHint: String?

    mutating func advance(to stage: ConnectStage, detail: String) {
        guard stage.rawValue >= self.stage.rawValue else { return }
        self.stage = stage
        self.detail = detail
        failure = nil
        failureHint = nil
    }

    mutating func fail(_ reason: String) {
        failure = reason
        failureHint = Self.hint(for: reason)
        detail = reason
    }

    static func hint(for reason: String) -> String {
        let text = reason.lowercased()
        if text.contains("protocol") {
            return "Update nanoctl on both this device and the host."
        }
        if text.contains("decoder") {
            return "The app will retry with a more compatible codec automatically."
        }
        if text.contains("host shut down") || text.contains("host ended") {
            return "Start the host agent again, then reconnect."
        }
        if text.contains("did not establish") {
            return "Check that the host is online, nanoctl is running, and Tailscale is connected."
        }
        if text.contains("connection lost") {
            return "The network path dropped. If this repeats, check Wi-Fi and Tailscale or lower the stream bitrate."
        }
        return "If this repeats, copy the debug log and compare versions on both sides."
    }
}
