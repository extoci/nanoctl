import AVFAudio
import Foundation
import OSLog
import UIKit
import VideoToolbox
import WebRTC

private let sessionLog = AppLogger(subsystem: "ai.nanoctl.viewer", category: "session")

enum CodecPreference: String, CaseIterable {
    /// Best efficient hardware-decoded codec first, falling back through H.264
    /// and then software-decoded choices, matching the browser viewer.
    case auto
    case hevc
    case h264
    case av1

    var label: String {
        switch self {
        case .auto: return "Auto"
        case .hevc: return "HEVC"
        case .h264: return "H264"
        case .av1: return "AV1"
        }
    }

    /// Whether this preference can use the Annex-B data-channel video path.
    var dataChannelCapable: Bool { true }
    var injectHevc: Bool { self == .auto || self == .hevc }

    var wireName: String? {
        switch self {
        case .auto: return nil
        case .hevc: return "hevc"
        case .h264: return "h264"
        case .av1: return "av1"
        }
    }

    /// Highest compression efficiency first, matching the browser viewer.
    static let efficiency = ["av1", "hevc", "h264"]

    /// Orders codecs by what both sides support, preferring hardware decode in
    /// auto mode. HEVC is representable through SDP injection even though the
    /// bundled WebRTC codec list cannot express it.
    static func orderedCodecs(
        preference: CodecPreference,
        hostCodecs: [String],
        receivableCodecs: [String],
        hardwareDecode: (String) -> Bool = hardwareDecodes
    ) -> [String] {
        var host = Set(hostCodecs.map { $0.lowercased() })
        host.insert("h264") // The agent's software H.264 fallback is always available.
        let receivable = Set(receivableCodecs.map { $0.lowercased() })
        let offered = efficiency.filter { codec in
            host.contains(codec) && (codec == "hevc" || receivable.contains(codec))
        }
        guard !offered.isEmpty else { return [] }
        if let explicit = preference.wireName, offered.contains(explicit) {
            return [explicit] + offered.filter { $0 != explicit }
        }
        let hardware = offered.filter { hardwareDecode($0) }
        return hardware
            + (offered.contains("h264") && !hardware.contains("h264") ? ["h264"] : [])
            + offered.filter { !hardware.contains($0) && $0 != "h264" }
    }

    static func hardwareDecodes(_ codec: String) -> Bool {
        switch codec {
        case "av1":
            if #available(iOS 26.2, *) {
                VTRegisterSupplementalVideoDecoderIfAvailable(kCMVideoCodecType_AV1)
            }
            return VTIsHardwareDecodeSupported(kCMVideoCodecType_AV1)
        case "hevc": return VTIsHardwareDecodeSupported(kCMVideoCodecType_HEVC)
        case "h264": return VTIsHardwareDecodeSupported(kCMVideoCodecType_H264)
        default: return false
        }
    }
}

enum VideoTransport {
    case rtp
    case dataChannel
}

@MainActor
final class Session {
    private let factory: RTCPeerConnectionFactory = {
        #if DEBUG && targetEnvironment(simulator)
        let decoder: RTCVideoDecoderFactory = LatencyProbe.enabled ? TimedDecoderFactory() : NanoctlDecoderFactory()
        #else
        let decoder = NanoctlDecoderFactory()
        #endif
        return RTCPeerConnectionFactory(
            encoderFactory: RTCDefaultVideoEncoderFactory(),
            decoderFactory: decoder,
            audioDevice: PlayoutOnlyAudioDevice()
        )
    }()
    private let network: URLSession
    private var connection: Connection?
    private var task: Task<Void, Never>?
    private var timeout: Task<Void, Never>?
    private var reconnectTask: Task<Void, Never>?
    private var statsTask: Task<Void, Never>?
    private(set) var endpoint: HostEndpoint?
    /// The user wants a session (connecting, live or retrying).
    private(set) var wantsConnection = false
    private var reconnectAttempts = 0

    private(set) var track: RTCVideoTrack?
    private(set) var audioTrack: RTCAudioTrack?
    private(set) var state = "Disconnected"
    private(set) var transport: VideoTransport = .rtp
    private(set) var codec: String?
    private(set) var audioOutputs: [AudioOutput] = []
    private(set) var selectedAudioOutput: String?
    private(set) var statsLine = ""
    let pixelMailbox = PixelMailbox()

    var codecPreference: CodecPreference = {
        CodecPreference(rawValue: UserDefaults.standard.string(forKey: "codec") ?? "") ?? .auto
    }() {
        didSet { UserDefaults.standard.set(codecPreference.rawValue, forKey: "codec") }
    }
    var audioEnabled = true {
        didSet {
            audioTrack?.isEnabled = audioEnabled
            _ = sendControlFree(["type": "audio", "enabled": audioEnabled])
        }
    }
    /// Viewer caps below the host's maximums; nil means the maximum.
    var maxFps: Int? { didSet { _ = setQuality(maxFps: maxFps, maxBitrateKbps: maxBitrateKbps) } }
    var maxBitrateKbps: Int? { didSet { _ = setQuality(maxFps: maxFps, maxBitrateKbps: maxBitrateKbps) } }
    var onChange: (() -> Void)?
    var onClipboard: ((String) -> Void)?

    var host: HostInfo? { connection?.host }
    var gamepadAllowed: Bool { connection?.host?.capabilities.gamepad == true }
    var inputReady: Bool {
        connection?.host?.canControl == true && connection?.control?.readyState == .open
    }

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
        self.endpoint = endpoint
        wantsConnection = true
        reconnectAttempts = 0
        connect(endpoint)
    }

    private func connect(_ endpoint: HostEndpoint) {
        teardown()
        var request = URLRequest(url: endpoint.socket)
        request.setValue(endpoint.origin, forHTTPHeaderField: "Origin")
        let socket = network.webSocketTask(with: request)
        socket.maximumMessageSize = 1_100_000
        let current = Connection(socket: socket)
        connection = current
        sessionLog.info("connecting to \(endpoint.socket.absoluteString)")
        state = "Connecting…"
        transport = .rtp
        sessionLog.notice("video transport switched to RTP")
        codec = nil
        audioOutputs = []
        onChange?()
        timeout = Task { [weak self, weak current] in
            try? await Task.sleep(for: .seconds(15))
            guard !Task.isCancelled, let self, let current, self.connection === current else { return }
            self.fail("The host did not establish a connection.", current)
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
                if connection === current { fail(error.localizedDescription, current) }
            }
        }
    }

    /// Reconnects to the last host, e.g. after a codec change or returning
    /// from the background.
    func restart() {
        if let endpoint { start(endpoint) }
    }

    /// Stops the connection and cancels auto-reconnect (user-initiated).
    func stop(_ status: String = "Disconnected") {
        sessionLog.info("stopped (\(status))")
        wantsConnection = false
        teardown()
        state = status
        onChange?()
    }

    /// Connection dropped or failed; schedules a retry while the user still
    /// wants the session.
    private func fail(_ reason: String, _ current: Connection) {
        guard connection === current else { return }
        sessionLog.error("connection failed: \(reason)")
        teardown()
        state = reason
        onChange?()
        guard wantsConnection else { return }
        reconnectAttempts += 1
        let delay = min(30, reconnectAttempts == 1 ? 1 : reconnectAttempts * 3)
        state = "\(reason) Reconnecting in \(delay)s…"
        onChange?()
        reconnectTask = Task { [weak self] in
            try? await Task.sleep(for: .seconds(delay))
            guard !Task.isCancelled, let self, self.wantsConnection, let endpoint = self.endpoint else { return }
            self.connect(endpoint)
        }
    }

    private func teardown() {
        let old = connection
        connection = nil
        task?.cancel()
        task = nil
        timeout?.cancel()
        timeout = nil
        statsTask?.cancel()
        statsTask = nil
        old?.socket.cancel(with: .normalClosure, reason: nil)
        old?.peer?.close()
        track = nil
        audioTrack = nil
        statsLine = ""
        pixelMailbox.put(nil)
    }

    @discardableResult
    func sendGamepad(_ report: GamepadMessage) -> Bool {
        // Gamepad reports are full-state snapshots: ride the unordered
        // zero-retransmit pointer channel so a lost report never delays the
        // next one. The agent accepts gamepad on this lane.
        guard gamepadAllowed else { return false }
        if connection?.host?.capabilities.binaryInput == true, let data = report.binary {
            return send(data, pointer: true, limitBacklog: true, binary: true)
        }
        guard let data = try? JSONEncoder().encode(report) else { return false }
        return send(data, pointer: true, limitBacklog: true)
    }

    @discardableResult
    func sendKey(_ down: Bool) -> Bool {
        sendControl(["type": "key", "action": down ? "down" : "up", "code": "Space",
                     "key": " ", "modifiers": 0, "repeat": false])
    }

    @discardableResult
    func sendText(_ text: String) -> Bool {
        sendControl(["type": "text", "text": text])
    }

    @discardableResult
    func sendRelease() -> Bool {
        sendControl(["type": "release"])
    }

    func sendPointer(x: Double, y: Double, action: String, button: Int = 0,
                     deltaX: Double = 0, deltaY: Double = 0) {
        if action == "move", connection?.host?.capabilities.binaryInput == true {
            var data = Data(count: 7)
            data[0] = 0x4e
            data[1] = 1
            data[2] = 1
            let xu = UInt16((min(1, max(0, x)) * 65535).rounded())
            let yu = UInt16((min(1, max(0, y)) * 65535).rounded())
            data[3] = UInt8(xu & 0xff)
            data[4] = UInt8(xu >> 8)
            data[5] = UInt8(yu & 0xff)
            data[6] = UInt8(yu >> 8)
            _ = send(data, pointer: true, limitBacklog: true, binary: true)
            return
        }
        var value: [String: Any] = ["type": "pointer", "action": action, "x": x, "y": y]
        if action == "wheel" {
            value["deltaX"] = deltaX
            value["deltaY"] = deltaY
        } else if action != "move" {
            value["button"] = button
        }
        guard let data = try? JSONSerialization.data(withJSONObject: value) else { return }
        _ = send(data, pointer: action == "move", limitBacklog: action == "move")
    }

    @discardableResult
    func selectDisplay(_ displayId: String) -> Bool {
        sendControl(["type": "display", "displayId": displayId])
    }

    @discardableResult
    func setQuality(maxFps: Int?, maxBitrateKbps: Int?) -> Bool {
        var value: [String: Any] = ["type": "quality"]
        if let maxFps { value["maxFps"] = maxFps }
        if let maxBitrateKbps { value["maxBitrateKbps"] = maxBitrateKbps }
        // Viewer quality limits are view-only safe on the host; they must not
        // depend on input admission.
        return sendControlFree(value)
    }

    @discardableResult
    func sendClipboard() -> Bool {
        guard let text = UIPasteboard.general.string, !text.isEmpty,
              connection?.clipboard?.readyState == .open else { return false }
        return sendClipboardText(text)
    }

    private func sendClipboardText(_ text: String) -> Bool {
        guard let data = try? JSONSerialization.data(withJSONObject: ["type": "clipboard", "text": text]),
              data.count < 64 * 1024, let channel = connection?.clipboard,
              channel.readyState == .open else { return false }
        return channel.sendData(RTCDataBuffer(data: data, isBinary: false))
    }

    @discardableResult
    func selectAudioOutput(_ outputId: String?) -> Bool {
        guard let socket = connection?.socket else { return false }
        var message: [String: Any] = ["type": "select-audio-output"]
        message["outputId"] = outputId ?? NSNull()
        guard let data = try? JSONSerialization.data(withJSONObject: message) else { return false }
        socket.send(.string(String(decoding: data, as: UTF8.self))) { _ in }
        return true
    }

    private func sendControl(_ value: [String: Any]) -> Bool {
        guard let data = try? JSONSerialization.data(withJSONObject: value) else { return false }
        return send(data)
    }

    private func send(_ data: Data, pointer: Bool = false, limitBacklog: Bool = false, binary: Bool = false) -> Bool {
        guard inputReady, let channel = pointer ? connection?.pointer : connection?.control,
              channel.readyState == .open, !limitBacklog || channel.bufferedAmount < 4096 else { return false }
        return channel.sendData(RTCDataBuffer(data: data, isBinary: binary))
    }

    /// Control-channel send that does not require input admission (used for
    /// ping, which is view-only safe).
    private func sendControlFree(_ value: [String: Any]) -> Bool {
        guard let data = try? JSONSerialization.data(withJSONObject: value),
              let channel = connection?.control, channel.readyState == .open else { return false }
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
            sessionLog.info("host hello: \(host.hostName) codecs=\(host.capabilities.codecs?.joined(separator: ",") ?? "?") audio=\(host.audioAvailable)")
            audioOutputs = host.capabilities.audioOutputs ?? []
            if audioOutputs.isEmpty == false { _ = selectAudioOutput(nil) }
            try await negotiate(current, injectHevc: codecPreference.injectHevc)
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
                let negotiated = SDP.negotiatedVideoCodec(sdp)
                let clean: RTCSessionDescription
                if negotiated == "hevc", let stripped = SDP.removePT(sdp, SDP.hevcPayloadType) {
                    // libwebrtc cannot express H265; describe the suppressed
                    // RTP leg with the remaining codecs instead.
                    clean = RTCSessionDescription(type: .answer, sdp: stripped)
                } else {
                    clean = RTCSessionDescription(type: .answer, sdp: sdp)
                }
                do {
                    try await peer.setRemoteDescription(clean)
                } catch {
                    if current.injectHevcAttempted {
                        sessionLog.error("HEVC negotiation failed; retrying as H.264")
                        // HEVC negotiation failed somewhere: retry as H.264 on
                        // a fresh peer.
                        current.peer?.close()
                        current.peer = nil
                        current.answered = false
                        try await negotiate(current, injectHevc: false)
                        return
                    }
                    throw error
                }
                try requireCurrent(current)
                current.answered = true
                codec = negotiated
                sessionLog.info("negotiated video codec: \(negotiated ?? "none")")
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
        case "audio-outputs":
            audioOutputs = message.outputs ?? []
            selectedAudioOutput = message.selectedId
            onChange?()
        case "end":
            stop(message.reason == "replaced" ? "Another viewer took over." : message.reason ?? "Session ended.")
        default:
            throw ViewerError("Unexpected host message.")
        }
    }

    private func negotiate(_ current: Connection, injectHevc: Bool) async throws {
        let delegate = PeerEvents()
        current.delegate = delegate
        delegate.onCandidate = { [weak self, weak current] payload in
            guard let self, let current, self.connection === current,
                  current.localSignals.count < 256 else { return }
            current.localSignals.append(payload)
            self.flush(current)
        }
        delegate.onTrack = { [weak self, weak current] receiver in
            guard let self, let current, self.connection === current else { return }
            if let video = receiver.track as? RTCVideoTrack {
                self.track = video
            } else if let audio = receiver.track as? RTCAudioTrack {
                self.audioTrack = audio
                audio.isEnabled = self.audioEnabled
            }
            self.onChange?()
        }
        delegate.onState = { [weak self, weak current] status in
            guard let self, let current, self.connection === current else { return }
            switch status {
            case .connected:
                self.timeout?.cancel()
                self.reconnectAttempts = 0
                self.state = "Live · \(current.host?.hostName ?? "nanoctl")"
                self.onChange?()
            case .failed, .closed:
                self.fail("Connection lost.", current)
            default:
                break
            }
        }
        delegate.onControl = { [weak self, weak current] in
            guard let self, let current, self.connection === current else { return }
            self.onControlOpen(current)
            self.onChange?()
        }
        delegate.onVideo = { [weak self, weak current] data in
            guard let self, let current, self.connection === current else { return }
            current.dcVideo?.receive(data)
        }
        delegate.onVideoOpen = { [weak self, weak current] in
            guard let self, let current, self.connection === current else { return }
            self.activateDataChannelVideo(current)
        }
        delegate.onClipboard = { [weak self, weak current] data in
            guard let self, let current, self.connection === current else { return }
            guard let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                  object["type"] as? String == "clipboard",
                  let text = object["text"] as? String else { return }
            UIPasteboard.general.string = text
            self.onClipboard?(text)
        }
        delegate.onChannelClose = { [weak self, weak current] channel in
            guard let self, let current, self.connection === current else { return }
            if channel === current.video || channel === current.videoIdr {
                self.deactivateDataChannelVideo(current)
            }
        }
        let configuration = RTCConfiguration()
        configuration.sdpSemantics = .unifiedPlan
        configuration.iceServers = []
        let constraints = RTCMediaConstraints(mandatoryConstraints: nil, optionalConstraints: nil)
        guard let peer = factory.peerConnection(with: configuration, constraints: constraints, delegate: delegate)
        else { throw ViewerError("Could not create a WebRTC peer.") }
        current.peer = peer

        let videoInit = RTCRtpTransceiverInit()
        videoInit.direction = .recvOnly
        guard let video = peer.addTransceiver(of: .video, init: videoInit) else {
            throw ViewerError("Could not add the video receiver.")
        }
        applyCodecPreferences(to: video, host: current.host)

        if current.host?.audioAvailable == true {
            let audio = RTCRtpTransceiverInit()
            audio.direction = .recvOnly
            _ = peer.addTransceiver(of: .audio, init: audio)
        }

        // Low-latency raw-video data channels. H.264/HEVC ride as Annex-B
        // access units and AV1 as low-overhead OBU temporal units; all three
        // decode through VideoToolbox on hardware that supports them.
        if codecPreference.dataChannelCapable {
            let videoConfig = RTCDataChannelConfiguration()
            videoConfig.isOrdered = false
            videoConfig.maxRetransmits = 0
            current.video = peer.dataChannel(forLabel: "nanoctl.video.v1", configuration: videoConfig)
            current.videoIdr = peer.dataChannel(forLabel: "nanoctl.video-idr.v1",
                                                configuration: RTCDataChannelConfiguration())
            current.video?.delegate = delegate
            current.videoIdr?.delegate = delegate
        }
        if current.host?.capabilities.clipboard == true {
            current.clipboard = peer.dataChannel(forLabel: "nanoctl.clipboard.v1",
                                                 configuration: RTCDataChannelConfiguration())
            current.clipboard?.delegate = delegate
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

        current.injectHevcAttempted = injectHevc
        let offer = try await peer.offer(for: constraints)
        try requireCurrent(current)
        try await peer.setLocalDescription(offer)
        try requireCurrent(current)
        // The local description stays what libwebrtc generated; only the wire
        // offer carries the injected H265 payload.
        let wire = injectHevc ? (SDP.injectHevc(offer.sdp, order: codecOrder(for: current.host)) ?? offer.sdp) : offer.sdp
        try await publish(SignalPayload(type: "offer", sdp: wire), current)
        current.offerSent = true
        flush(current)
    }

    /// Orders the video transceiver's codec preferences for the RTP path;
    /// auxiliary payloads (rtx/red/fec) keep their positions at the end.
    private func codecOrder(for host: HostInfo?) -> [String] {
        let receivable = factory.rtpReceiverCapabilities(forKind: "video").codecs.map { $0.name }
        return CodecPreference.orderedCodecs(
            preference: codecPreference,
            hostCodecs: host?.capabilities.codecs ?? ["h264"],
            receivableCodecs: receivable
        )
    }

    private func applyCodecPreferences(to transceiver: RTCRtpTransceiver, host: HostInfo?) {
        var caps = factory.rtpReceiverCapabilities(forKind: "video").codecs
        guard !caps.isEmpty else { return }
        let order = codecOrder(for: host)
        let auxiliary = caps.filter {
            let name = $0.name.lowercased()
            return name == "rtx" || name == "red" || name == "fec" || name == "ulpfec"
        }
        let media = caps.filter { cap in
            !auxiliary.contains { $0.name == cap.name }
        }
        let rank: (RTCRtpCodecCapability) -> Int = { cap in
            order.firstIndex(of: cap.name.lowercased()) ?? order.count
        }
        let sortedMedia = media.enumerated().sorted { left, right in
            let leftRank = rank(left.element)
            let rightRank = rank(right.element)
            return leftRank == rightRank ? left.offset < right.offset : leftRank < rightRank
        }
        caps = sortedMedia.map { $0.element } + auxiliary
        // The error-taking overload is not visible to Swift in stasel's build;
        // codec preference failures are non-fatal either way.
        transceiver.setCodecPreferences(caps)
    }

    /// Both video data channels must be open before the host will route
    /// keyframes to the IDR channel and accept transport opt-out.
    private func activateDataChannelVideo(_ current: Connection) {
        guard let video = current.video, video.readyState == .open,
              let idr = current.videoIdr, idr.readyState == .open,
              current.dcVideo == nil else { return }
        let receiver = DCVideoReceiver(codec: codec.flatMap(VTDecoder.Codec.init(rawValue:)))
        receiver.requestKeyframe = { [weak video] in
            video?.sendData(RTCDataBuffer(data: Data("KF".utf8), isBinary: false))
        }
        receiver.onBuffer = { [weak self] buffer in
            self?.pixelMailbox.put(buffer)
        }
        receiver.onPersistentFault = { [weak self, weak current] in
            Task { @MainActor in
                guard let session = self, let current = current ?? session.connection else { return }
                current.video?.close()
                current.videoIdr?.close()
                session.deactivateDataChannelVideo(current)
            }
        }
        receiver.onUnsupported = { [weak self, weak current] in
            // The wire stream is none of H.264/HEVC/AV1: close the data
            // channels so the host keeps the RTP leg, which libwebrtc
            // decodes for every negotiated codec.
            Task { @MainActor in
                guard let session = self, let current = current ?? session.connection else { return }
                session.deactivateDataChannelVideo(current)
            }
        }
        current.dcVideo = receiver
        transport = .dataChannel
        sessionLog.info("data-channel video active: codec=\(receiver.codec?.rawValue ?? "pending")")
        // The agent suppresses delta-frame RTP writes automatically while the
        // video channel is open; IDRs still flow so RTCP feedback survives.
        onControlOpen(current)
        onChange?()
    }

    private func deactivateDataChannelVideo(_ current: Connection) {
        guard current.dcVideo != nil || transport == .dataChannel else { return }
        current.dcVideo = nil
        transport = .rtp
        pixelMailbox.put(nil)
        onChange?()
    }

    private func onControlOpen(_ current: Connection) {
        guard current.control?.readyState == .open else { return }
        _ = sendControlFree(["type": "audio", "enabled": audioEnabled])
        if !current.sentDisplayQualityCap {
            current.sentDisplayQualityCap = true
            _ = setQuality(maxFps: UIScreen.main.maximumFramesPerSecond, maxBitrateKbps: nil)
        }
        guard statsTask == nil else { return }
        if maxFps != nil || maxBitrateKbps != nil { _ = setQuality(maxFps: maxFps, maxBitrateKbps: maxBitrateKbps) }
        statsTask = nil
        statsTask = Task { [weak self, weak current] in
            var ticks = 0
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(1))
                guard let self, let current, self.connection === current else { return }
                ticks += 1
                if ticks % 3 == 0 {
                    _ = self.sendControlFree(["type": "ping",
                                            "nonce": Int(Date().timeIntervalSince1970 * 1000),
                                            "sentAt": Int(Date().timeIntervalSince1970 * 1000)])
                }
                await self.collectStats(current)
            }
        }
    }

    /// Refreshes the status line and watches for a decoder path that never
    /// produces frames so it can fall back to RTP.
    private func collectStats(_ current: Connection) async {
        guard let receiver = current.dcVideo else {
            statsLine = codec.map { "RTP · \($0)" } ?? ""
            onChange?()
            return
        }
        let (_, transit, frames) = receiver.takeLinkStats()
        statsLine = "DC · \(receiver.codec?.rawValue ?? codec ?? "?") · \(frames) fps · \(String(format: "%.0f", transit))ms · drop \(receiver.droppedCount)"
        onChange?()

        // Fallback: if the decoder faulted and never produced a frame after
        // plenty of traffic, restore RTP so the user still gets video.
        if receiver.decodedCount == 0, receiver.stats.msgs > 90, receiver.hasFaulted {
            current.video?.close()
            current.videoIdr?.close()
            deactivateDataChannelVideo(current)
        }
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
                if connection === current { fail(error.localizedDescription, current) }
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
    var video: RTCDataChannel?
    var videoIdr: RTCDataChannel?
    var clipboard: RTCDataChannel?
    var dcVideo: DCVideoReceiver?
    var remoteCandidates: [RTCIceCandidate] = []
    var localSignals: [SignalPayload] = []
    var offerSent = false
    var answered = false
    var flushing = false
    var sequence = 0
    var injectHevcAttempted = false
    var sentDisplayQualityCap = false

    init(socket: URLSessionWebSocketTask) { self.socket = socket }
}

private final class PeerEvents: NSObject, RTCPeerConnectionDelegate, RTCDataChannelDelegate {
    var onCandidate: ((SignalPayload) -> Void)?
    var onTrack: ((RTCRtpReceiver) -> Void)?
    var onState: ((RTCPeerConnectionState) -> Void)?
    var onControl: (() -> Void)?
    var onVideo: ((Data) -> Void)?
    var onVideoOpen: (() -> Void)?
    var onClipboard: ((Data) -> Void)?
    var onChannelClose: ((RTCDataChannel) -> Void)?

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
        DispatchQueue.main.async { [weak self] in self?.onTrack?(rtpReceiver) }
    }

    func peerConnection(_ peer: RTCPeerConnection, didChange state: RTCPeerConnectionState) {
        DispatchQueue.main.async { [weak self] in self?.onState?(state) }
    }

    func dataChannelDidChangeState(_ channel: RTCDataChannel) {
        DispatchQueue.main.async { [weak self] in
            switch channel.readyState {
            case .open:
                if channel.label == "nanoctl.control.v1" {
                    self?.onControl?()
                } else if channel.label == "nanoctl.video.v1" || channel.label == "nanoctl.video-idr.v1" {
                    self?.onVideoOpen?()
                }
            case .closed:
                self?.onChannelClose?(channel)
            default:
                break
            }
        }
    }

    func dataChannel(_ channel: RTCDataChannel, didReceiveMessageWith buffer: RTCDataBuffer) {
        let data = buffer.data
        switch channel.label {
        case "nanoctl.video.v1", "nanoctl.video-idr.v1":
            onVideo?(data)
        case "nanoctl.clipboard.v1":
            DispatchQueue.main.async { [weak self] in self?.onClipboard?(data) }
        default:
            break
        }
    }

    func peerConnection(_ peer: RTCPeerConnection, didChange state: RTCSignalingState) {}
    func peerConnection(_ peer: RTCPeerConnection, didChange state: RTCIceConnectionState) {}
    func peerConnection(_ peer: RTCPeerConnection, didAdd stream: RTCMediaStream) {}
    func peerConnection(_ peer: RTCPeerConnection, didRemove stream: RTCMediaStream) {}
    func peerConnectionShouldNegotiate(_ peer: RTCPeerConnection) {}
    func peerConnection(_ peer: RTCPeerConnection, didRemove candidates: [RTCIceCandidate]) {}
    func peerConnection(_ peer: RTCPeerConnection, didOpen channel: RTCDataChannel) {}
}
