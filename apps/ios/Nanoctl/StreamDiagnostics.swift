import Foundation

/// Compact, operator-oriented stream measurements shown in the stats overlay.
struct StreamDiagnostics: Equatable {
    var transport: String
    var codec: String? = nil
    var resolution: String? = nil
    var fps: Int? = nil
    var bitrateKbps: Int? = nil
    var rttMs: Double? = nil
    var path: String? = nil
    var lossPercent: Double? = nil
    var decoder: String? = nil
    var dropped: Int? = nil

    var overlayText: String {
        let video = [transport, codec, resolution, fps.map { "\($0)fps" }]
            .compactMap { $0 }.joined(separator: " · ")
        let network = [
            bitrateKbps.map { Self.bitrateLabel(kbps: $0) },
            rttMs.map { Self.rttLabel(ms: $0) },
            path,
            lossPercent.map { "loss \(String(format: "%.1f", $0))%" }
        ].compactMap { $0 }.joined(separator: " · ")
        let decode = [decoder, dropped.map { "drop \($0)" }]
            .compactMap { $0 }.joined(separator: " · ")
        return [video, network, decode].filter { !$0.isEmpty }.joined(separator: "\n")
    }

    static func pathLabel(localType: String?, remoteType: String?, networkType: String?, tailnet: Bool) -> String? {
        var parts: [String] = []
        if tailnet { parts.append("tailnet") }
        if let networkType, networkType != "unknown" { parts.append(networkType) }
        if let localType, let remoteType {
            let route = localType == "relay" || remoteType == "relay" ? "relay" : "direct"
            parts.append("\(route) \(localType)→\(remoteType)")
        }
        return parts.isEmpty ? nil : parts.joined(separator: " ")
    }

    static func rttLabel(ms: Double) -> String {
        ms < 1 ? "<1ms RTT" : "\(Int(ms.rounded()))ms RTT"
    }

    static func bitrateLabel(kbps: Int) -> String {
        if kbps >= 1000 {
            return "\(String(format: "%.1f", Double(kbps) / 1000)) Mbps"
        }
        return "\(kbps) kbps"
    }

}
