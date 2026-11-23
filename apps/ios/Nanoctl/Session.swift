import Foundation
import WebRTC

@MainActor
final class Session {
    private let factory: RTCPeerConnectionFactory = {
        #if DEBUG && targetEnvironment(simulator)
        let decoder: RTCVideoDecoderFactory = LatencyProbe.enabled ? TimedDecoderFactory() : RTCDefaultVideoDecoderFactory()
        #else
        let decoder = RTCDefaultVideoDecoderFactory()
        #endif
        return RTCPeerConnectionFactory(encoderFactory: RTCDefaultVideoEncoderFactory(), decoderFactory: decoder)
    }()
    private let network: URLSession
    private var connection: Connection?
    private var task: Task<Void, Never>?
    private var timeout: Task<Void, Never>?
    private(set) var track: RTCVideoTrack?
    private(set) var state = "Disconnected"
    var gamepadAllowed: Bool { connection?.host?.capabilities.gamepad == true }
    var inputReady: Bool {
        connection?.host?.canControl == true && connection?.control?.readyState == .open
    }
    var onChange: (() -> Void)?

    #if DEBUG && targetEnvironment(simulator)
    func receiverStats() async -> ReceiverStats? {
        guard let peer = connection?.peer else { return nil }
        return await withCheckedContinuation { reply in
            peer.statistics { report in
                let stream = report.statistics.values.first {
                    $0.type == "inbound-rtp" && ($0.values["kind"] as? String) == "video"
                }
                reply.resume(returning: stream.map { ReceiverStats($0.values) })
            }
        }
    }
    #endif

    init() {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 15
        configuration.requestCachePolicy = .reloadIgnoringLocalCacheData
        network = URLSession(configuration: configuration, delegate: NoRedirects(), delegateQueue: nil)
    }

    func start(_ endpoint: HostEndpoint) {
        stop()
        var request = URLRequest(url: endpoint.socket)
        request.setValue(endpoint.origin, forHTTPHeaderField: "Origin")
        let socket = network.webSocketTask(with: request)
        socket.maximumMessageSize = 1_100_000
        let current = Connection(socket: socket)
        connection = current
        state = "Connecting…"
        onChange?()
        timeout = Task { [weak self, weak current] in
            try? await Task.sleep(for: .seconds(15))
            guard !Task.isCancelled, let self, let current, self.connection === current else { return }
            self.stop("The host did not establish a connection.")
        }
        task = Task { [weak self] in
            guard let self else { return }
            do {
                let (data, response) = try await network.data(from: endpoint.base.appending(path: "v1/info"))
                guard let http = response as? HTTPURLResponse, http.statusCode == 200 else {
                    throw ViewerError("Host authorization or discovery failed (HTTP \((response as? HTTPURLResponse)?.statusCode ?? 0)).")
                }
                try Self.checkHost(JSONDecoder().decode(HostInfo.self, from: data))
                try requireCurrent(current)
                socket.resume()
                while !Task.isCancelled {
                    let message = try await socket.receive()
                    try requireCurrent(current)
                    guard case .string(let json) = message else { throw ViewerError("Expected JSON from host.") }
                    try await receive(JSONDecoder().decode(ServerMessage.self, from: Data(json.utf8)), current)
                }
            } catch {
                if connection === current { stop(error.localizedDescription) }
            }
        }
    }

    func stop(_ status: String = "Disconnected") {
        let old = connection
        connection = nil
        task?.cancel()
        task = nil
        timeout?.cancel()
        timeout = nil
        old?.socket.cancel(with: .normalClosure, reason: nil)
        old?.peer?.close()
        track = nil
        state = status
        onChange?()
    }

    @discardableResult
    func sendGamepad(_ report: GamepadMessage) -> Bool {
        guard gamepadAllowed, let data = try? JSONEncoder().encode(report) else { return false }
        return send(data, limitBacklog: true)
    }

    @discardableResult
    func sendKey(_ down: Bool) -> Bool {
        sendControl(["type": "key", "action": down ? "down" : "up", "code": "Space",
                     "key": " ", "modifiers": 0, "repeat": false])
    }

    func sendPointer(x: Double, y: Double, action: String) {
        var value: [String: Any] = ["type": "pointer", "action": action, "x": x, "y": y]
        if action != "move" { value["button"] = 0 }
        guard let data = try? JSONSerialization.data(withJSONObject: value) else { return }
        _ = send(data, pointer: action == "move", limitBacklog: action == "move")
    }

    private func sendControl(_ value: [String: Any]) -> Bool {
        guard let data = try? JSONSerialization.data(withJSONObject: value) else { return false }
        return send(data)
    }

    private func send(_ data: Data, pointer: Bool = false, limitBacklog: Bool = false) -> Bool {
        guard inputReady, let channel = pointer ? connection?.pointer : connection?.control,
              channel.readyState == .open, !limitBacklog || channel.bufferedAmount < 4096 else { return false }
        return channel.sendData(RTCDataBuffer(data: data, isBinary: false))
    }

    private func requireCurrent(_ current: Connection) throws {
        if connection !== current || Task.isCancelled { throw CancellationError() }
    }

    private static func checkHost(_ host: HostInfo) throws {
        guard host.capabilities.protocolVersion == SignalEnvelope.wireVersion else {
            throw ViewerError("This viewer requires agent protocol 2.")
        }
    }

    private func receive(_ message: ServerMessage, _ current: Connection) async throws {
        switch message.type {
        case "hello":
            guard current.id == nil, let id = message.sessionId, !id.isEmpty, let host = message.host else {
                throw ViewerError("Invalid host hello.")
            }
            try Self.checkHost(host)
            current.id = id
            current.host = host
            try await negotiate(current)
        case "signal":
            guard let id = current.id, let peer = current.peer, let signal = message.envelope,
                  signal.belongs(to: id, controller: current.epoch, generation: 0) else { return }
            guard current.hostEpoch == nil || current.hostEpoch == signal.hostEpoch else {
                throw ViewerError("Host restarted. Connect again.")
            }
            current.hostEpoch = signal.hostEpoch
            switch signal.payload.type {
            case "answer":
                guard !current.answered, let sdp = signal.payload.sdp, !sdp.isEmpty else { return }
                try await peer.setRemoteDescription(RTCSessionDescription(type: .answer, sdp: sdp))
                try requireCurrent(current)
                current.answered = true
                let candidates = current.remoteCandidates
                current.remoteCandidates.removeAll()
                for candidate in candidates { try await peer.add(candidate) }
            case "ice-candidate":
                guard let sdp = signal.payload.candidate, !sdp.isEmpty,
                      let index = signal.payload.sdpMLineIndex, index >= 0 else { return }
                let candidate = RTCIceCandidate(sdp: sdp, sdpMLineIndex: index, sdpMid: signal.payload.sdpMid)
                if current.answered { try await peer.add(candidate) }
                else if current.remoteCandidates.count < 256 { current.remoteCandidates.append(candidate) }
            case "end":
                stop(signal.payload.reason ?? "Session ended.")
            default:
                break
            }
        case "end":
            stop(message.reason == "replaced" ? "Another viewer took over." : message.reason ?? "Session ended.")
        default:
            throw ViewerError("Unexpected host message.")
        }
    }

    private func negotiate(_ current: Connection) async throws {
        let delegate = PeerEvents()
        current.delegate = delegate
        delegate.onCandidate = { [weak self, weak current] payload in
            guard let self, let current, self.connection === current,
                  current.localSignals.count < 256 else { return }
            current.localSignals.append(payload)
            self.flush(current)
        }
        delegate.onTrack = { [weak self, weak current] track in
            guard let self, let current, self.connection === current else { return }
            self.track = track
            self.onChange?()
        }
        delegate.onState = { [weak self, weak current] status in
            guard let self, let current, self.connection === current else { return }
            if status == .connected {
                self.timeout?.cancel()
                self.state = "Live · \(current.host?.hostName ?? "nanoctl")"
                self.onChange?()
            } else if status == .failed || status == .closed {
                self.stop("Connection lost. Connect again.")
            }
        }
        delegate.onControl = { [weak self, weak current] in
            guard let self, let current, self.connection === current else { return }
            self.onChange?()
        }
        let configuration = RTCConfiguration()
        configuration.sdpSemantics = .unifiedPlan
        configuration.iceServers = []
        let constraints = RTCMediaConstraints(mandatoryConstraints: nil, optionalConstraints: nil)
        guard let peer = factory.peerConnection(with: configuration, constraints: constraints, delegate: delegate)
        else { throw ViewerError("Could not create a WebRTC peer.") }
        current.peer = peer
        let video = RTCRtpTransceiverInit()
        video.direction = .recvOnly
        guard peer.addTransceiver(of: .video, init: video) != nil else {
            throw ViewerError("Could not add the video receiver.")
        }

        let pointer = RTCDataChannelConfiguration()
        pointer.isOrdered = false
        pointer.maxRetransmits = 0
        current.pointer = peer.dataChannel(forLabel: "nanoctl.pointer.v1", configuration: pointer)
        current.control = peer.dataChannel(forLabel: "nanoctl.control.v1", configuration: RTCDataChannelConfiguration())
        guard let control = current.control, current.pointer != nil else {
            throw ViewerError("Could not create the input channels.")
        }
        control.delegate = delegate

        let offer = try await peer.offer(for: constraints)
        try requireCurrent(current)
        try await peer.setLocalDescription(offer)
        try requireCurrent(current)
        try await publish(SignalPayload(type: "offer", sdp: offer.sdp), current)
        current.offerSent = true
        flush(current)
    }

    private func flush(_ current: Connection) {
        guard current.offerSent, !current.flushing, !current.localSignals.isEmpty else { return }
        current.flushing = true
        Task {
            defer { current.flushing = false }
            do {
                while connection === current && !current.localSignals.isEmpty {
                    try await publish(current.localSignals.removeFirst(), current)
                }
            } catch {
                if connection === current { stop(error.localizedDescription) }
            }
        }
    }

    private func publish(_ payload: SignalPayload, _ current: Connection) async throws {
        guard let id = current.id else { throw ViewerError("Missing session ID.") }
        let envelope = SignalEnvelope(
            version: SignalEnvelope.wireVersion, sessionId: id, controllerEpoch: current.epoch, hostEpoch: "",
            generation: 0, sequence: current.sequence, sender: "controller",
            sentAt: Int64(Date().timeIntervalSince1970 * 1000), payload: payload
        )
        current.sequence += 1
        let data = try JSONEncoder().encode(ClientSignal(envelope: envelope))
        try await current.socket.send(.string(String(decoding: data, as: UTF8.self)))
    }
}

private final class NoRedirects: NSObject, URLSessionTaskDelegate {
    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) {
        completionHandler(nil)
    }
}

private final class Connection {
    let socket: URLSessionWebSocketTask
    let epoch = UUID().uuidString
    var id: String?
    var host: HostInfo?
    var hostEpoch: String?
    var peer: RTCPeerConnection?
    var delegate: PeerEvents?
    var control: RTCDataChannel?
    var pointer: RTCDataChannel?
    var remoteCandidates: [RTCIceCandidate] = []
    var localSignals: [SignalPayload] = []
    var offerSent = false
    var answered = false
    var flushing = false
    var sequence = 0

    init(socket: URLSessionWebSocketTask) { self.socket = socket }
}

private final class PeerEvents: NSObject, RTCPeerConnectionDelegate, RTCDataChannelDelegate {
    var onCandidate: ((SignalPayload) -> Void)?
    var onTrack: ((RTCVideoTrack) -> Void)?
    var onState: ((RTCPeerConnectionState) -> Void)?
    var onControl: (() -> Void)?

    func peerConnection(_ peer: RTCPeerConnection, didGenerate candidate: RTCIceCandidate) {
        let payload = SignalPayload(type: "ice-candidate", candidate: candidate.sdp,
                                    sdpMid: candidate.sdpMid, sdpMLineIndex: candidate.sdpMLineIndex)
        DispatchQueue.main.async { [weak self] in self?.onCandidate?(payload) }
    }

    func peerConnection(_ peer: RTCPeerConnection, didChange state: RTCIceGatheringState) {
        if state == .complete {
            DispatchQueue.main.async { [weak self] in self?.onCandidate?(SignalPayload(type: "ice-complete")) }
        }
    }

    func peerConnection(_ peer: RTCPeerConnection, didAdd rtpReceiver: RTCRtpReceiver, streams: [RTCMediaStream]) {
        if let track = rtpReceiver.track as? RTCVideoTrack {
            DispatchQueue.main.async { [weak self] in self?.onTrack?(track) }
        }
    }

    func peerConnection(_ peer: RTCPeerConnection, didChange state: RTCPeerConnectionState) {
        DispatchQueue.main.async { [weak self] in self?.onState?(state) }
    }

    func dataChannelDidChangeState(_ channel: RTCDataChannel) {
        DispatchQueue.main.async { [weak self] in self?.onControl?() }
    }

    func dataChannel(_ channel: RTCDataChannel, didReceiveMessageWith buffer: RTCDataBuffer) {}
    func peerConnection(_ peer: RTCPeerConnection, didChange state: RTCSignalingState) {}
    func peerConnection(_ peer: RTCPeerConnection, didChange state: RTCIceConnectionState) {}
    func peerConnection(_ peer: RTCPeerConnection, didAdd stream: RTCMediaStream) {}
    func peerConnection(_ peer: RTCPeerConnection, didRemove stream: RTCMediaStream) {}
    func peerConnectionShouldNegotiate(_ peer: RTCPeerConnection) {}
    func peerConnection(_ peer: RTCPeerConnection, didRemove candidates: [RTCIceCandidate]) {}
    func peerConnection(_ peer: RTCPeerConnection, didOpen channel: RTCDataChannel) {}
}
