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

    /// Highest compression efficiency first — except AV1, whose data-channel
    /// bitstream path has never once been verified decoding on a device;
    /// HEVC leads auto until AV1 is proven end to end.
    static let efficiency = ["hevc", "av1", "h264"]

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

private func statNumber(_ stat: RTCStatistics?, _ key: String) -> Double? {
    (stat?.values[key] as? NSNumber)?.doubleValue
}

private func statString(_ stat: RTCStatistics?, _ key: String) -> String? {
    stat?.values[key] as? String
}

/// libwebrtc exposes candidate network types as its adapter enum on some
/// builds and as strings on others; the enum values are stable ABI constants.
private func networkTypeName(_ stat: RTCStatistics?) -> String? {
    if let name = statString(stat, "networkType") { return name }
    switch (stat?.values["networkType"] as? NSNumber)?.intValue {
    case 1: return "ethernet"
    case 2: return "wifi"
    case 3: return "cellular"
    case 4: return "vpn"
    case 5: return "loopback"
    default: return nil
    }
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
    /// Session-local codec degradation. A persistent HEVC decode fault must not
    /// leave the app black or permanently overwrite the user's codec preference.
    private var forceH264 = false
    private var restoringStreamSettings = false

    private(set) var track: RTCVideoTrack?
    private(set) var audioTrack: RTCAudioTrack?
    private(set) var state = "Disconnected"
    private(set) var connectProgress = ConnectProgress()
    private(set) var transport: VideoTransport = .rtp
    private(set) var codec: String?
    private(set) var audioOutputs: [AudioOutput] = []
    private(set) var selectedAudioOutput: String?
    private(set) var statsLine = ""
    private var lastLoggedGaps = -1
    private var lastLoggedKf = -1
    let pixelMailbox = PixelMailbox()

    var codecPreference: CodecPreference = {
        CodecPreference(rawValue: UserDefaults.standard.string(forKey: "codec") ?? "") ?? .auto
    }() {
        didSet {
            guard !restoringStreamSettings else { return }
            persistStreamSettings()
        }
    }
    var audioEnabled = true {
        didSet {
            audioTrack?.isEnabled = audioEnabled
            _ = sendControlBytesFree([9, audioEnabled ? 1 : 0])
        }
    }
    private var viewportWidth: Int?
    private var viewportHeight: Int?
    private var gamepadSequence: UInt8 = 0
    /// Viewer caps below the host's maximums; nil means the maximum.
    var maxFps: Int? {
        didSet {
            guard !restoringStreamSettings else { return }
            persistStreamSettings()
            _ = setQuality(maxFps: maxFps, maxBitrateKbps: maxBitrateKbps)
        }
    }
    var maxBitrateKbps: Int? {
        didSet {
            guard !restoringStreamSettings else { return }
            persistStreamSettings()
            _ = setQuality(maxFps: maxFps, maxBitrateKbps: maxBitrateKbps)
        }
    }
    var onChange: (() -> Void)?
    var onClipboard: ((String) -> Void)?

    var host: HostInfo? { connection?.host }
    var displayMaxFps: Int {
        let maximums = UIApplication.shared.connectedScenes.compactMap {
            ($0 as? UIWindowScene)?.screen.maximumFramesPerSecond
        }
        return max(60, maximums.max() ?? 60)
    }
    var gamepadAllowed: Bool { connection?.host?.capabilities.gamepad == true }
    var inputReady: Bool {
        connection?.host?.canControl == true && connection?.control?.readyState == .open
    }

    #if DEBUG && targetEnvironment(simulator)
    func diagnostics() -> SessionDiagnostics {
        let dc = connection?.dcVideo?.statusSnapshot()
        return SessionDiagnostics(
            state: state,
            transport: transport == .dataChannel ? "data-channel" : "rtp",
            codec: dc?.codec?.rawValue ?? codec,
            inputReady: inputReady,
            dcMessages: dc?.stats.msgs ?? 0,
            dcFed: dc?.stats.fed ?? 0,
            dcDecoded: dc?.decoded ?? 0,
            dcDropped: dc?.dropped ?? 0,
            dcGaps: dc?.stats.gaps ?? 0,
            dcKeyframeRequests: dc?.stats.kfRequests ?? 0,
            dcFaulted: dc?.faulted ?? false,
            dcKeys: dc?.stats.keys ?? 0,
            dcExpectedSeq: dc?.expectedSeq,
            dcWaitingForKey: dc?.waitingForKey ?? true,
            dcReordered: dc?.reorderedCount ?? 0,
            dcPending: dc?.pendingCount ?? 0
        )
    }

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

    private func restoreStreamSettings(for endpoint: HostEndpoint) {
        let stored = StreamSettingsStore().settings(for: endpoint.base.absoluteString)
        let fallbackCodec = CodecPreference(rawValue: UserDefaults.standard.string(forKey: "codec") ?? "") ?? .auto
        restoringStreamSettings = true
        codecPreference = stored.flatMap { CodecPreference(rawValue: $0.codec) } ?? fallbackCodec
        maxFps = stored?.maxFps
        maxBitrateKbps = stored?.maxBitrateKbps
        restoringStreamSettings = false
        if stored == nil {
            // Freeze the inherited defaults for this host at first use. Later
            // changes made for another machine must not silently alter an
            // existing host merely because the user never opened its settings.
            persistStreamSettings()
        }
    }

    private func persistStreamSettings() {
        guard let endpoint else { return }
        StreamSettingsStore().save(HostStreamSettings(codec: codecPreference.rawValue,
                                                      maxFps: maxFps,
                                                      maxBitrateKbps: maxBitrateKbps),
                                   for: endpoint.base.absoluteString)
    }

    func start(_ endpoint: HostEndpoint) {
        self.endpoint = endpoint
        restoreStreamSettings(for: endpoint)
        wantsConnection = true
        reconnectAttempts = 0
        forceH264 = false
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
        connectProgress = ConnectProgress()
        transport = .rtp
        sessionLog.notice("video transport switched to RTP")
        codec = nil
        audioOutputs = []
        onChange?()
        timeout = Task { [weak self, weak current] in
            // Progress-driven, not a fixed budget: signaling alone can take
            // seconds on a slow path and connectivity checks retransmit
            // silently, so any static limit either strangles slow-but-working
            // attempts or leaves wedged ones hanging. Any state transition,
            // candidate, track or channel event is proof of life; the attempt
            // only dies after twelve seconds of total silence.
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(2))
                guard !Task.isCancelled, let self, let current, self.connection === current else { return }
                if Date().timeIntervalSince(current.lastProgress) > 12 {
                    sessionLog.error("no connection progress for 12s; abandoning attempt")
                    self.fail("The host did not establish a connection.", current)
                    return
                }
            }
        }
        // Signaling runs detached from the main actor: reconnects previously
        // inherited main-actor isolation, so while the UI thread was still
        // busy tearing the old session down the new connection could not even
        // start — codec switches visibly stalled for about a second before
        // the first byte moved, on loopback and on real networks alike.
        task = Task.detached { [weak self] in
            guard let self else { return }
            do {
                // The WebSocket hello already carries the complete HostInfo
                // payload. Fetching /v1/info first cost one full TCP/TLS round
                // trip before signaling could even start; Moonlight-style fast
                // attach starts the only required channel immediately.
                socket.resume()
                while !Task.isCancelled {
                    let message = try await socket.receive()
                    guard case .string(let json) = message else { throw ViewerError("Expected JSON from host.") }
                    let decoded = try JSONDecoder().decode(ServerMessage.self, from: Data(json.utf8))
                    try await self.receive(decoded, current)
                }
            } catch {
                await self.failIfCurrent(error.localizedDescription, current)
            }
        }
    }

    private func applyNetworkAdapterPolicy(_ endpoint: HostEndpoint) {
        let options = RTCPeerConnectionFactoryOptions()
        options.ignoreLoopbackNetworkAdapter = !endpoint.isLoopback
        if endpoint.usesTailnetMediaPath {
            // Tailscale is a utun/VPN adapter on iOS. WebRTC otherwise gathers
            // every Wi-Fi/cellular interface and lets their higher network
            // priority win even though the host only publishes tailnet
            // candidates; those pairs can transmit into the tunnel but never
            // complete the return path. Keep VPN and unknown adapters while
            // dropping the physical classes that cannot carry this media.
            options.ignoreWiFiNetworkAdapter = true
            options.ignoreCellularNetworkAdapter = true
            options.ignoreEthernetNetworkAdapter = true
            sessionLog.info("ice adapter policy: tailnet interfaces only")
        } else {
            sessionLog.info("ice adapter policy: direct interfaces")
        }
        factory.setOptions(options)
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
    private func failIfCurrent(_ reason: String, _ current: Connection) {
        if connection === current { fail(reason, current) }
    }

    private func fail(_ reason: String, _ current: Connection) {
        guard connection === current else { return }
        sessionLog.error("connection failed: \(reason)")
        teardown()
        state = reason
        connectProgress.fail(reason)
        onChange?()
        guard wantsConnection else { return }
        reconnectAttempts += 1
        let delay = min(30, reconnectAttempts == 1 ? 1 : reconnectAttempts * 3)
        state = "\(reason) Reconnecting in \(delay)s…"
        connectProgress.detail = state
        onChange?()
        reconnectTask = Task { [weak self] in
            try? await Task.sleep(for: .seconds(delay))
            guard !Task.isCancelled, let self, self.wantsConnection, let endpoint = self.endpoint else { return }
            self.connect(endpoint)
        }
    }

    private func teardown() {
        reconnectTask?.cancel()
        reconnectTask = nil
        let old = connection
        connection = nil
        task?.cancel()
        task = nil
        timeout?.cancel()
        timeout = nil
        statsTask?.cancel()
        statsTask = nil
        old?.statePoll?.cancel()
        old?.iceWatchdog?.cancel()
        old?.socket.cancel(with: .normalClosure, reason: nil)
        // libwebrtc runs every peer connection of the factory on one shared
        // signaling thread, and close() occupies that thread for about a
        // second while the SCTP association shuts down — long enough to stall
        // the replacement session's offer/answer negotiation behind it, which
        // is why codec switches and reconnects idled for a second before the
        // first candidate appeared. Data channels do not use the signaling
        // thread once established, so deferring the close past the
        // negotiation window hides it entirely.
        if let peer = old?.peer {
            Task.detached {
                try? await Task.sleep(for: .seconds(3))
                peer.close()
            }
        }
        track = nil
        audioTrack = nil
        statsLine = ""
        pixelMailbox.put(nil)
    }

    @discardableResult
    func sendGamepad(_ report: GamepadMessage, sparse: Bool = false) -> Bool {
        // Sparse reports self-repair individual slots and use the bounded-loss
        // gamepad lane. Full reports are the sparse baseline, so they stay
        // reliable even when they carry pads.
        let wire = sparse ? report.sparseBinary(sequence: gamepadSequence) : report.binary(sequence: gamepadSequence)
        guard gamepadAllowed, let data = wire else { return false }
        guard send(data, pointer: false, gamepad: sparse && !report.pads.isEmpty, limitBacklog: true, binary: true) else { return false }
        gamepadSequence &+= 1
        return true
    }

    func sendGamepadHeartbeat() -> Bool {
        guard gamepadAllowed else { return false }
        return send(Data([11]), pointer: false, gamepad: true, limitBacklog: true, binary: true)
    }

    private func appendPosition(_ data: inout Data, x: Double, y: Double) {
        let xu = UInt16((min(1, max(0, x)) * 65535).rounded())
        let yu = UInt16((min(1, max(0, y)) * 65535).rounded())
        data.append(contentsOf: [UInt8(xu & 0xff), UInt8(xu >> 8), UInt8(yu & 0xff), UInt8(yu >> 8)])
    }

    /// Significant big-endian IEEE-754 bytes; trailing zero bytes are omitted
    /// and restored by the host, preserving the exact double bit pattern.
    private func compactF64Bytes(_ value: Double) -> [UInt8] {
        var bytes = stride(from: 56, through: 0, by: -8).map {
            UInt8((value.bitPattern >> UInt64($0)) & 0xff)
        }
        while bytes.last == 0 { bytes.removeLast() }
        return bytes
    }

    @discardableResult
    func sendKey(_ down: Bool, key: RemoteKey, repeating: Bool = false) -> Bool {
        guard let data = key.binary(down: down, repeating: repeating) else { return false }
        return send(data, binary: true)
    }

    @discardableResult
    func sendSpaceKey(_ down: Bool) -> Bool {
        guard let space = RemoteKey.special("Space") else { return false }
        return sendKey(down, key: space)
    }

    @discardableResult
    func sendText(_ text: String) -> Bool {
        let bytes = Array(text.utf8)
        guard bytes.count <= 65535 else { return false }
        var data = Data([6, UInt8(bytes.count & 0xff), UInt8(bytes.count >> 8)])
        data.append(contentsOf: bytes)
        return send(data, binary: true)
    }

    @discardableResult
    func sendRelease() -> Bool {
        send(Data([7]), binary: true)
    }

    func sendPointer(x: Double, y: Double, action: String, button: Int = 0,
                     deltaX: Double = 0, deltaY: Double = 0) {
        var data = Data()
        if action == "move" {
            data.append(contentsOf: [1])
            appendPosition(&data, x: x, y: y)
        } else if action == "wheel" {
            let xBytes = compactF64Bytes(deltaX)
            let yBytes = compactF64Bytes(deltaY)
            data.append(contentsOf: [4])
            appendPosition(&data, x: x, y: y)
            data.append(contentsOf: [UInt8(xBytes.count | (yBytes.count << 4))])
            data.append(contentsOf: xBytes)
            data.append(contentsOf: yBytes)
        } else {
            guard action == "down" || action == "up", (0...2).contains(button) else { return }
            data.append(contentsOf: [3])
            let flags = UInt8(action == "up" ? 0x80 : 0) | UInt8(button)
            data.append(contentsOf: [flags])
            appendPosition(&data, x: x, y: y)
        }
        _ = send(data, pointer: action == "move", limitBacklog: action == "move", binary: true)
    }

    @discardableResult
    func selectDisplay(_ displayId: String) -> Bool {
        let bytes = Array(displayId.utf8)
        guard bytes.count <= 255 else { return false }
        var data = Data([10, UInt8(bytes.count)])
        data.append(contentsOf: bytes)
        return send(data, binary: true)
    }

    /// Reports the physical pixels visible to the rendered video. Nil restores
    /// the host's configured dimensions while zoomed or using external output.
    func setViewportSize(width: Int?, height: Int?) {
        guard width != viewportWidth || height != viewportHeight else { return }
        viewportWidth = width
        viewportHeight = height
        _ = setQuality(maxFps: maxFps, maxBitrateKbps: maxBitrateKbps)
    }

    @discardableResult
    func setQuality(maxFps: Int?, maxBitrateKbps: Int?) -> Bool {
        var value: [String: Any] = ["type": "quality"]
        if let maxFps { value["maxFps"] = maxFps }
        if let maxBitrateKbps { value["maxBitrateKbps"] = maxBitrateKbps }
        if let viewportWidth { value["maxWidth"] = viewportWidth }
        if let viewportHeight { value["maxHeight"] = viewportHeight }
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
        selectedAudioOutput = outputId
        onChange?()
        return true
    }

    private func sendControl(_ value: [String: Any]) -> Bool {
        guard let data = try? JSONSerialization.data(withJSONObject: value) else { return false }
        return send(data)
    }

    private func send(_ data: Data, pointer: Bool = false, gamepad: Bool = false, limitBacklog: Bool = false, binary: Bool = false) -> Bool {
        guard inputReady, let channel = gamepad ? connection?.gamepad : pointer ? connection?.pointer : connection?.control,
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

    /// Binary control-channel send that does not require input admission.
    private func sendControlBytesFree(_ bytes: [UInt8]) -> Bool {
        guard let channel = connection?.control, channel.readyState == .open else { return false }
        return channel.sendData(RTCDataBuffer(data: Data(bytes), isBinary: true))
    }

    private func requireCurrent(_ current: Connection) throws {
        if connection !== current || Task.isCancelled { throw CancellationError() }
    }

    private func hostEnded(_ reason: String?, _ current: Connection) {
        switch reason {
        case "replaced":
            stop("Another viewer took over.")
        case "viewer-ended":
            stop("Session ended.")
        default:
            // A service restart or host-side media/transport failure is not a
            // user intent to give up. Treat the server-sent terminal message as
            // a recoverable drop and use the same bounded backoff as ICE loss.
            let text = reason == "shutdown" ? "Host shut down." : "Host ended the session\(reason.map { " (\($0))" } ?? "")."
            fail(text, current)
        }
    }

    private nonisolated static func checkHost(_ host: HostInfo) throws {
        guard host.capabilities.protocolVersion == SignalEnvelope.wireVersion else {
            throw ViewerError("This viewer requires agent protocol 2.")
        }
    }

    private func receive(_ message: ServerMessage, _ current: Connection) async throws {
        try requireCurrent(current)
        switch message.type {
        case "hello":
            guard current.id == nil, let id = message.sessionId, !id.isEmpty, let host = message.host else {
                throw ViewerError("Invalid host hello.")
            }
            try Self.checkHost(host)
            current.id = id
            current.host = host
            sessionLog.info("host hello: \(host.hostName) codecs=\(host.capabilities.codecs?.joined(separator: ",") ?? "?") audio=\(host.audioAvailable)")
            connectProgress.advance(to: .signaling, detail: "Host answered. Negotiating stream…")
            audioOutputs = host.capabilities.audioOutputs ?? []
            if audioOutputs.isEmpty == false { _ = selectAudioOutput(nil) }
            try await negotiate(current, injectHevc: codecPreference.injectHevc && !forceH264)
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
                connectProgress.advance(to: .networking, detail: "Checking network paths…")
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
                hostEnded(signal.payload.reason, current)
            default:
                break
            }
        case "audio-outputs":
            audioOutputs = message.outputs ?? []
            selectedAudioOutput = message.selectedId
            onChange?()
        case "end":
            hostEnded(message.reason, current)
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
            current.lastProgress = Date()
            if let video = receiver.track as? RTCVideoTrack {
                sessionLog.info("video track arrived (RTP leg)")
                self.track = video
            } else if let audio = receiver.track as? RTCAudioTrack {
                self.audioTrack = audio
                audio.isEnabled = self.audioEnabled
            }
            self.onChange?()
        }
        delegate.onState = { [weak self, weak current] status in
            guard let self, let current, self.connection === current else { return }
            current.lastProgress = Date()
            sessionLog.info("peer connection state: \(peerStateName(status))")
            switch status {
            case .connected:
                self.timeout?.cancel()
                self.reconnectAttempts = 0
                current.disconnectedSince = nil
                self.connectProgress.advance(to: .media, detail: "Network connected. Opening media channels…")
                self.onChange?()
            case .disconnected:
                // ICE commonly flaps for a moment on Wi-Fi/cellular transitions.
                // Give the transport a bounded chance to recover, but never let
                // a silent black frame masquerade as a live session.
                if current.disconnectedSince == nil {
                    current.disconnectedSince = Date()
                    Task { [weak self, weak current] in
                        try? await Task.sleep(for: .seconds(6))
                        guard !Task.isCancelled, let self, let current,
                              self.connection === current,
                              current.disconnectedSince != nil else { return }
                        self.fail("Connection lost.", current)
                    }
                }
            case .failed, .closed:
                self.fail("Connection lost.", current)
            default:
                break
            }
        }
        delegate.onIceState = { [weak self, weak current] state in
            guard let self, let current, self.connection === current else { return }
            sessionLog.info("ice connection state: \(iceStateName(state))")
            current.lastProgress = Date()
            if state == .connected || state == .completed {
                self.timeout?.cancel()
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
        if let endpoint { applyNetworkAdapterPolicy(endpoint) }
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
            // Time-bounded retransmission repairs isolated SCTP loss for a
            // fraction of an IDR's cost; unordered delivery prevents head-of-line
            // blocking, and expired copies would be too late for the reorder window.
            videoConfig.maxPacketLifeTime = 80
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
        let gamepad = RTCDataChannelConfiguration()
        gamepad.isOrdered = false
        // Sparse reports are sequenced absolute state, so bounded retransmission
        // cannot regress newer input and replaces most always-on duplicates.
        gamepad.maxPacketLifeTime = 100
        current.gamepad = peer.dataChannel(forLabel: "nanoctl.gamepad.v1", configuration: gamepad)
        current.control = peer.dataChannel(forLabel: "nanoctl.control.v1", configuration: RTCDataChannelConfiguration())
        guard let control = current.control, current.pointer != nil, current.gamepad != nil else {
            throw ViewerError("Could not create the input channels.")
        }
        control.delegate = delegate

        // libwebrtc does not reliably deliver state callbacks on every
        // network path: over some transports the SCTP channels are provably
        // open (host traffic answers on them) while the delegate never fires
        // and ICE sits in "checking" until the connect timeout kills a
        // working session. Poll the actual channel state and activate on
        // evidence rather than event delivery. The activation paths are
        // idempotent, so overlapping delegate delivery is harmless.
        current.statePoll = Task { [weak self, weak current] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .milliseconds(250))
                guard !Task.isCancelled, let self, let current, self.connection === current else { return }
                if current.video?.readyState == .open || current.control?.readyState == .open {
                    // Any open channel proves DTLS/SCTP is alive regardless of
                    // what the ICE state machine reports.
                    self.timeout?.cancel()
                    self.reconnectAttempts = 0
                }
                if current.control?.readyState == .open {
                    self.onControlOpen(current)
                }
                self.activateDataChannelVideo(current)
                if current.video?.readyState == .open && current.videoIdr?.readyState == .open,
                   current.control?.readyState == .open {
                    return
                }
            }
        }

        current.injectHevcAttempted = injectHevc
        let offer = try await peer.offer(for: constraints)
        try requireCurrent(current)
        try await peer.setLocalDescription(offer)
        try requireCurrent(current)
        // The local description stays what libwebrtc generated; only the wire
        // offer carries the injected H265 payload.
        let wire = injectHevc ? (SDP.injectHevc(offer.sdp, order: codecOrder(for: current.host)) ?? offer.sdp) : offer.sdp
        try await publish(SignalPayload(type: "offer", sdp: wire), current)
        connectProgress.advance(to: .signaling, detail: "Waiting for host answer…")
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
        receiver.requestKeyframe = { [weak idr] in
            // The delta lane is deliberately lossy exactly when recovery is
            // needed. Match the browser viewer: repair requests ride the
            // reliable IDR lane; the host accepts them on either channel.
            idr?.sendData(RTCDataBuffer(data: Data("KF".utf8), isBinary: false))
        }
        receiver.onBuffer = { [weak self, weak current] buffer in
            self?.pixelMailbox.put(buffer)
            Task { @MainActor in
                guard let self, let current, self.connection === current else { return }
                self.markMediaLive(current, decodedFrames: 1)
            }
        }
        receiver.onPersistentFault = { [weak self, weak current] in
            Task { @MainActor in
                guard let session = self, let current = current ?? session.connection else { return }
                session.dataChannelDecoderFailed(current)
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
        receiver.begin()
        transport = .dataChannel
        connectProgress.advance(to: .media, detail: "Video channel open. Waiting for first frame…")
        sessionLog.info("data-channel video active: codec=\(receiver.codec?.rawValue ?? "pending")")
        // The agent suppresses delta-frame RTP writes automatically while the
        // video channel is open; IDRs still flow so RTCP feedback survives.
        onControlOpen(current)
        onChange?()
    }

    func resetCodecFallback() {
        forceH264 = false
    }

    /// RTP frames share the thin keepalive leg while data-channel video is
    /// active, so only the RTP transport may use them as first-media evidence.
    func markRtpMediaLive() {
        guard transport == .rtp, let connection else { return }
        markMediaLive(connection, decodedFrames: 1)
    }

    /// Decoder failure is not a terminal UX state. If HEVC itself is the
    /// suspect, retry once as H.264; otherwise reanimate the RTP leg. Either
    /// outcome is better than a trustworthy-looking black screen.
    private func dataChannelDecoderFailed(_ current: Connection) {
        guard connection === current else { return }
        if codec == "hevc", !forceH264 {
            forceH264 = true
            sessionLog.error("HEVC decoder persistently faulted; retrying as H.264")
            state = "HEVC decoder failed. Retrying with H.264…"
            if let endpoint { connect(endpoint) }
            return
        }
        current.video?.close()
        current.videoIdr?.close()
        deactivateDataChannelVideo(current)
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
        _ = sendControlBytesFree([9, audioEnabled ? 1 : 0])
        if !current.sentDisplayQualityCap {
            current.sentDisplayQualityCap = true
            _ = setQuality(maxFps: displayMaxFps, maxBitrateKbps: nil)
        }
        guard statsTask == nil else { return }
        if maxFps != nil || maxBitrateKbps != nil { _ = setQuality(maxFps: maxFps, maxBitrateKbps: maxBitrateKbps) }
        statsTask = nil
        statsTask = Task { [weak self, weak current] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(1))
                guard let self, let current, self.connection === current else { return }
                await self.collectStats(current)
            }
        }
    }

    private func markMediaLive(_ current: Connection, decodedFrames: Int) {
        guard connection === current, decodedFrames > 0, connectProgress.stage != .live else { return }
        connectProgress.advance(to: .live, detail: "Live")
        state = "Live · \(current.host?.hostName ?? "nanoctl")"
        onChange?()
    }

    private func statisticsReport(_ peer: RTCPeerConnection) async -> RTCStatisticsReport {
        await withCheckedContinuation { reply in
            peer.statistics { report in reply.resume(returning: report) }
        }
    }

    /// Refreshes the diagnostics overlay and watches for a decoder path that
    /// never produces frames so it can fall back to RTP.
    private func collectStats(_ current: Connection) async {
        var inbound: RTCStatistics?
        var rttMs: Double?
        var path: String?
        if let peer = current.peer {
            let report = await statisticsReport(peer)
            let all = Array(report.statistics.values)
            inbound = all.first {
                $0.type == "inbound-rtp" && ((statString($0, "kind") ?? statString($0, "mediaType")) == "video")
            }
            let transport = all.first { $0.type == "transport" && statString($0, "selectedCandidatePairId") != nil }
            let selectedId = transport.flatMap { statString($0, "selectedCandidatePairId") }
            let selected = selectedId.flatMap { report.statistics[$0] }
            let nominated = all.filter {
                $0.type == "candidate-pair" && statString($0, "state") == "succeeded"
                    && (statNumber($0, "nominated") ?? 0) != 0
            }
            let pair = selected?.type == "candidate-pair" ? selected : nominated.first
            if let pair {
                if let rtt = statNumber(pair, "currentRoundTripTime"), rtt > 0 {
                    rttMs = rtt * 1000
                }
                let local = statString(pair, "localCandidateId").flatMap { report.statistics[$0] }
                let remote = statString(pair, "remoteCandidateId").flatMap { report.statistics[$0] }
                path = StreamDiagnostics.pathLabel(
                    localType: statString(local, "candidateType"),
                    remoteType: statString(remote, "candidateType"),
                    networkType: networkTypeName(local),
                    tailnet: endpoint?.usesTailnetMediaPath == true
                )
            }
        }

        guard let receiver = current.dcVideo else {
            var diagnostics = StreamDiagnostics(transport: "RTP", codec: codec, rttMs: rttMs, path: path)
            if let inbound {
                let width = statNumber(inbound, "frameWidth") ?? 0
                let height = statNumber(inbound, "frameHeight") ?? 0
                if width > 0, height > 0 { diagnostics.resolution = "\(Int(width))×\(Int(height))" }
                diagnostics.decoder = statString(inbound, "decoderImplementation")
                let bytes = Int(statNumber(inbound, "bytesReceived") ?? 0)
                let frames = Int(statNumber(inbound, "framesDecoded") ?? 0)
                markMediaLive(current, decodedFrames: frames)
                let received = Int(statNumber(inbound, "packetsReceived") ?? 0)
                let lost = Int(statNumber(inbound, "packetsLost") ?? 0)
                let now = Date()
                if let before = current.rtpStatsAt {
                    let elapsed = now.timeIntervalSince(before)
                    if elapsed > 0 {
                        diagnostics.bitrateKbps = Int((Double(bytes - current.rtpStatsBytes) * 8 / elapsed / 1000).rounded())
                        diagnostics.fps = Int((Double(frames - current.rtpStatsFrames) / elapsed).rounded())
                        let receivedDelta = received - current.rtpStatsReceived
                        let lostDelta = lost - current.rtpStatsLost
                        if receivedDelta + lostDelta > 0 {
                            diagnostics.lossPercent = Double(max(0, lostDelta)) / Double(receivedDelta + lostDelta) * 100
                        }
                    }
                }
                current.rtpStatsAt = now
                current.rtpStatsBytes = bytes
                current.rtpStatsFrames = frames
                current.rtpStatsReceived = received
                current.rtpStatsLost = lost
            }
            statsLine = diagnostics.overlayText
            onChange?()
            return
        }

        let status = receiver.statusSnapshot()
        markMediaLive(current, decodedFrames: status.decoded)
        let wire = status.stats
        let (loss, transit, frames) = receiver.takeLinkStats()
        // Receiver-measured loss is the host's only congestion signal while
        // video rides the data channel: RTP feedback describes the thin
        // keepalive leg, not real egress, so the rate controller adapts to
        // these reports instead — the difference between a stream that
        // degrades gracefully and one that collapses on a bad link.
        _ = sendControlFree(["type": "link-stats", "loss": loss])
        let now = Date()
        var bitrateKbps: Int?
        if let before = current.dcStatsAt {
            let elapsed = now.timeIntervalSince(before)
            if elapsed > 0 {
                bitrateKbps = Int((Double(wire.bytes - current.dcStatsBytes) * 8 / elapsed / 1000).rounded())
            }
        }
        current.dcStatsAt = now
        current.dcStatsBytes = wire.bytes
        var diagnostics = StreamDiagnostics(
            transport: "DC",
            codec: status.codec?.rawValue ?? codec,
            fps: frames,
            bitrateKbps: bitrateKbps,
            rttMs: rttMs,
            path: path,
            lossPercent: loss * 100,
            decoder: "VideoToolbox",
            dropped: status.dropped
        )
        if status.width > 0, status.height > 0 { diagnostics.resolution = "\(status.width)×\(status.height)" }
        statsLine = diagnostics.overlayText
        onChange?()
        // Loss/gap visibility is what distinguishes a congested or relayed
        // transport from decoder trouble in the field; log it once a second
        // only when there is something to say.
        if loss > 0.01 || wire.gaps != lastLoggedGaps || wire.kfRequests != lastLoggedKf {
            lastLoggedGaps = wire.gaps
            lastLoggedKf = wire.kfRequests
            sessionLog.info("dc link: loss \(String(format: "%.1f", loss * 100))% gaps \(wire.gaps) kf \(wire.kfRequests) fed \(wire.fed) decoded \(status.decoded) transit \(String(format: "%.0f", transit))ms")
        }

        // Moonlight-style liveness supervision: never trust an open channel as
        // proof of video. Silence means the transport is lying to us; accepted
        // access units without decoded output mean the decoder is wedged. Both
        // paths fall back instead of leaving a black screen until the user gives up.
        let messagesDelta = wire.msgs - current.lastVideoMessages
        let fedDelta = wire.fed - current.lastVideoFed
        let decodedDelta = status.decoded - current.lastVideoDecoded
        current.lastVideoMessages = wire.msgs
        current.lastVideoFed = wire.fed
        current.lastVideoDecoded = status.decoded
        current.videoSilentSeconds = messagesDelta == 0 ? current.videoSilentSeconds + 1 : 0
        // A steady stream of wire messages with no decoded output is still a
        // black screen: packet loss can leave the receiver waiting for repair
        // even while the lossy delta lane itself looks lively.
        current.decoderStallSeconds = decodedDelta == 0 && (fedDelta > 0 || messagesDelta > 20)
            ? current.decoderStallSeconds + 1
            : 0
        if current.videoSilentSeconds >= 8 {
            sessionLog.error("data-channel video silent for 8s; falling back to RTP")
            current.video?.close()
            current.videoIdr?.close()
            deactivateDataChannelVideo(current)
            return
        }
        if current.decoderStallSeconds >= 3 || (status.decoded == 0 && wire.msgs > 90 && status.faulted) {
            sessionLog.error("data-channel decoder produced no frames; recovering video")
            dataChannelDecoderFailed(current)
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

private func peerStateName(_ state: RTCPeerConnectionState) -> String {
    switch state {
    case .new: return "new"
    case .connecting: return "connecting"
    case .connected: return "connected"
    case .disconnected: return "disconnected"
    case .failed: return "failed"
    case .closed: return "closed"
    @unknown default: return "unknown"
    }
}

private func iceStateName(_ state: RTCIceConnectionState) -> String {
    switch state {
    case .new: return "new"
    case .checking: return "checking"
    case .connected: return "connected"
    case .completed: return "completed"
    case .failed: return "failed"
    case .disconnected: return "disconnected"
    case .closed: return "closed"
    case .count: return "count"
    @unknown default: return "unknown"
    }
}

private func channelStateName(_ state: RTCDataChannelState) -> String {
    switch state {
    case .connecting: return "connecting"
    case .open: return "open"
    case .closing: return "closing"
    case .closed: return "closed"
    @unknown default: return "unknown"
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
    var gamepad: RTCDataChannel?
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
    var disconnectedSince: Date?
    var lastVideoMessages = 0
    var lastVideoFed = 0
    var lastVideoDecoded = 0
    var rtpStatsAt: Date?
    var rtpStatsBytes = 0
    var rtpStatsFrames = 0
    var rtpStatsReceived = 0
    var rtpStatsLost = 0
    var dcStatsAt: Date?
    var dcStatsBytes = 0
    var videoSilentSeconds = 0
    var decoderStallSeconds = 0
    var statePoll: Task<Void, Never>?
    var iceWatchdog: Task<Void, Never>?
    var lastProgress = Date()

    init(socket: URLSessionWebSocketTask) { self.socket = socket }
}

private final class PeerEvents: NSObject, RTCPeerConnectionDelegate, RTCDataChannelDelegate {
    var onCandidate: ((SignalPayload) -> Void)?
    var onTrack: ((RTCRtpReceiver) -> Void)?
    var onState: ((RTCPeerConnectionState) -> Void)?
    var onIceState: ((RTCIceConnectionState) -> Void)?
    var onControl: (() -> Void)?
    var onVideo: ((Data) -> Void)?
    var onVideoOpen: (() -> Void)?
    var onClipboard: ((Data) -> Void)?
    var onChannelClose: ((RTCDataChannel) -> Void)?

    func peerConnection(_ peer: RTCPeerConnection, didGenerate candidate: RTCIceCandidate) {
        let payload = SignalPayload(type: "ice-candidate", candidate: candidate.sdp,
                                    sdpMid: candidate.sdpMid, sdpMLineIndex: candidate.sdpMLineIndex)
        // Candidate-type visibility is what separates "no usable pair ever
        // existed" from "pairs exist but checks die" in field logs. Redact
        // the address; the type and protocol are the diagnostic content.
        let sdp = candidate.sdp
        let kind = sdp.components(separatedBy: "typ ").last?.components(separatedBy: " ").first ?? "?"
        let transport = sdp.contains(" udp ") ? "udp" : sdp.contains(" tcp ") ? "tcp" : "?"
        // A `.local` address means the candidate is mDNS-concealed: a host
        // that cannot resolve mDNS (or form peer-reflexive pairs) can never
        // complete checks against it, which is precisely the failure shape
        // seen against the Windows agent.
        let concealment = sdp.contains(".local") ? "mdns" : "ip"
        sessionLog.info("ice candidate: \(kind)/\(transport)/\(concealment)")
        DispatchQueue.main.async { [weak self] in self?.onCandidate?(payload) }
    }

    func peerConnection(_ peer: RTCPeerConnection, didChange state: RTCIceGatheringState) {
        if state == .complete {
            sessionLog.info("ice gathering complete")
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
            sessionLog.info("data channel \(channel.label): \(channelStateName(channel.readyState))")
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
    func peerConnection(_ peer: RTCPeerConnection, didChange state: RTCIceConnectionState) {
        DispatchQueue.main.async { [weak self] in self?.onIceState?(state) }
    }
    func peerConnection(_ peer: RTCPeerConnection, didAdd stream: RTCMediaStream) {}
    func peerConnection(_ peer: RTCPeerConnection, didRemove stream: RTCMediaStream) {}
    func peerConnectionShouldNegotiate(_ peer: RTCPeerConnection) {}
    func peerConnection(_ peer: RTCPeerConnection, didRemove candidates: [RTCIceCandidate]) {}
    func peerConnection(_ peer: RTCPeerConnection, didOpen channel: RTCDataChannel) {}
}
