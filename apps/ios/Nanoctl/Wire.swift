import Foundation

struct HostEndpoint {
    let base: URL
    let socket: URL
    let origin: String

    init(_ address: String, allowSimulatorLoopback: Bool = false) throws {
        let text = address.trimmingCharacters(in: .whitespacesAndNewlines)
        guard var parts = URLComponents(string: text.contains("://") ? text : "https://\(text)"),
              let host = parts.host, !host.isEmpty,
              parts.user == nil, parts.password == nil, parts.query == nil, parts.fragment == nil,
              parts.path.isEmpty || parts.path == "/",
              parts.scheme == "https" ||
                (allowSimulatorLoopback && parts.scheme == "http" && ["127.0.0.1", "localhost"].contains(host)),
              (1...65535).contains(parts.port ?? 7337)
        else { throw ViewerError("Enter an HTTPS host address, optionally with a port.") }
        parts.port = parts.port ?? 7337
        parts.path = ""
        guard let base = parts.url else { throw ViewerError("Invalid host address.") }
        self.base = base
        origin = base.absoluteString
        parts.scheme = parts.scheme == "https" ? "wss" : "ws"
        parts.path = "/v1/connect"
        guard let socket = parts.url else { throw ViewerError("Invalid WebSocket address.") }
        self.socket = socket
    }
}

struct ViewerError: LocalizedError {
    let message: String
    init(_ message: String) { self.message = message }
    var errorDescription: String? { message }
}

struct DisplayInfo: Decodable, Identifiable {
    let id: String
    let name: String
    let width: Int
    let height: Int
    let scaleFactor: Double?
    let primary: Bool?
}

struct AudioOutput: Decodable, Identifiable {
    let id: String
    let name: String
    let isDefault: Bool?
}

struct HostInfo: Decodable {
    struct Viewer: Decodable { let mayControl: Bool }
    struct Capabilities: Decodable {
        let protocolVersion: Int
        let gamepad: Bool?
        let codecs: [String]?
        let systemAudio: Bool?
        let audioOutputs: [AudioOutput]?
        let input: Bool?
        let clipboard: Bool?
        let displays: [DisplayInfo]?
        let binaryInput: Bool?
    }

    let hostName: String
    let platform: String?
    let version: String?
    let displays: [DisplayInfo]?
    let remoteInput: Bool
    let viewer: Viewer
    let capabilities: Capabilities
    var canControl: Bool { remoteInput && viewer.mayControl }
    var displayList: [DisplayInfo] { displays ?? capabilities.displays ?? [] }
    var audioAvailable: Bool { capabilities.systemAudio == true || capabilities.audioOutputs != nil }
}

struct SignalPayload: Codable {
    let type: String
    var sdp: String? = nil
    var candidate: String? = nil
    var sdpMid: String? = nil
    var sdpMLineIndex: Int32? = nil
    var reason: String? = nil
}

struct SignalEnvelope: Codable {
    static let wireVersion = 2
    let version: Int
    let sessionId: String
    let controllerEpoch: String
    let hostEpoch: String
    let generation: Int
    let sequence: Int
    let sender: String
    let sentAt: Int64
    let payload: SignalPayload

    func belongs(to session: String, controller: String, generation: Int) -> Bool {
        version == Self.wireVersion && sender == "host" && !hostEpoch.isEmpty &&
        sessionId == session && controllerEpoch == controller && self.generation == generation
    }
}

struct ServerMessage: Decodable {
    let type: String
    let sessionId: String?
    let host: HostInfo?
    let envelope: SignalEnvelope?
    let reason: String?
    let outputs: [AudioOutput]?
    let selectedId: String?
}

struct ClientSignal: Encodable {
    let type = "signal"
    let envelope: SignalEnvelope
}

struct GamepadMessage: Encodable, Equatable {
    struct DualShock4: Encodable, Equatable {
        let slot: Int
        let leftTriggerPressed: Bool
        let rightTriggerPressed: Bool
        let touchpadPressed: Bool
    }

    let type = "gamepad"
    let pads: [[Int]]
    var dualShock4: [DualShock4]? = nil

    /// Binary input v1 (`N`, 1, 2, count, 14 bytes per pad), matching the
    /// viewer and the agent's `Snapshot::parse_binary`; nil when it cannot
    /// represent the report so the caller can fall back to JSON.
    var binary: Data? {
        guard pads.count <= 4, pads.allSatisfy({ $0.count == 8 }) else { return nil }
        let sony = Dictionary((dualShock4 ?? []).map { ($0.slot, $0) }, uniquingKeysWith: { $1 })
        var data = Data([0x4e, 1, 2, UInt8(pads.count)])
        for pad in pads {
            guard let slot = UInt8(exactly: pad[0]), let buttons = UInt16(exactly: pad[1]),
                  let left = UInt8(exactly: pad[2]), let right = UInt8(exactly: pad[3]) else { return nil }
            var axes = [UInt16]()
            for value in pad[4...] {
                guard let axis = Int16(exactly: value) else { return nil }
                axes.append(UInt16(bitPattern: axis))
            }
            var flags: UInt8 = 0
            if let ds4 = sony[pad[0]] {
                flags = 1
                if ds4.leftTriggerPressed { flags |= 2 }
                if ds4.rightTriggerPressed { flags |= 4 }
                if ds4.touchpadPressed { flags |= 8 }
            }
            data.append(contentsOf: [slot, UInt8(buttons & 0xff), UInt8(buttons >> 8), left, right])
            for axis in axes { data.append(contentsOf: [UInt8(axis & 0xff), UInt8(axis >> 8)]) }
            data.append(flags)
        }
        return data
    }

    static func axis(_ value: Float) -> Int {
        guard value.isFinite else { return 0 }
        let unit = min(1, max(-1, value))
        return Int((unit * (unit < 0 ? 32768 : 32767)).rounded())
    }

    static func trigger(_ value: Float) -> Int {
        guard value.isFinite else { return 0 }
        return Int((min(1, max(0, value)) * 255).rounded())
    }
}
