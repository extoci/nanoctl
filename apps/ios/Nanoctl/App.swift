import AVFoundation
import Observation
import SwiftUI
import UIKit
import WebRTC

@main
@MainActor
final class App: UIResponder, UIApplicationDelegate {
    let desktop = Desktop()
}

/// What the SwiftUI shell renders; refreshed from the session on every change.
@MainActor
@Observable
final class ViewerModel {
    var active = false
    var hasVideo = false
    var external = false
    var status = "Disconnected"
    var hostName: String?
    var stats = ""
    var audioAvailable = false
    var audioEnabled = true
    var clipboard = false
    var displays: [DisplayInfo] = []
    var toast: String?
    var touchMode = TouchMode.trackpad
    var keyboardVisible = false
    var codec = CodecPreference.auto
    var maxFps: Int?
    var maxBitrateKbps: Int?
    var showStats = UserDefaults.standard.bool(forKey: "showStats") {
        didSet { UserDefaults.standard.set(showStats, forKey: "showStats") }
    }
    var recents = UserDefaults.standard.stringArray(forKey: "recentHosts")
        ?? UserDefaults.standard.string(forKey: "host").map { [$0] } ?? []
    var discoveredHosts: [DiscoveredHost] = []
    var discoveringHosts = false
    var discoveryNotice: String?
    var gamepads: [GamepadStatus] = []
}

@MainActor
final class Desktop {
    let session = Session()
    let mailbox = FrameMailbox()
    let model = ViewerModel()
    lazy var gamepad = Gamepad(session: session)
    lazy var stage = Stage(desktop: self)
    weak var external: VideoView?
    private var track: RTCVideoTrack?
    private var toastTask: Task<Void, Never>?
    private var airPlayCheck: Task<Void, Never>?
    private var discoveryTask: Task<Void, Never>?
    #if DEBUG && targetEnvironment(simulator)
    var probe: LatencyProbe?
    #endif

    init() {
        session.onChange = { [weak self] in self?.update() }
        gamepad.onStatuses = { [weak self] statuses in
            self?.model.gamepads = statuses
        }
        stage.onKeyboardVisible = { [weak self] visible in
            self?.model.keyboardVisible = visible
        }
        NotificationCenter.default.addObserver(forName: AVAudioSession.routeChangeNotification, object: nil,
                                               queue: .main) { [weak self] _ in
            MainActor.assumeIsolated { self?.checkAirPlay() }
        }
    }

    var hasVideo: Bool { session.track != nil || session.transport == .dataChannel }

    func update() {
        if track !== session.track {
            track?.remove(mailbox)
            mailbox.renderFrame(nil)
            track = session.track
            track?.add(mailbox)
        }
        stage.update(external: external != nil)
        let pixels = session.transport == .dataChannel ? session.pixelMailbox : nil
        external?.show(track == nil ? nil : mailbox)
        external?.show(pixels: pixels)
        UIApplication.shared.isIdleTimerDisabled = hasVideo

        let host = session.host
        model.active = session.wantsConnection
        model.hasVideo = hasVideo
        model.external = external != nil
        model.status = session.state
        model.hostName = host?.hostName ?? session.endpoint?.base.host()
        model.stats = session.statsLine
        model.audioAvailable = host?.audioAvailable == true
        model.audioEnabled = session.audioEnabled
        model.clipboard = host?.capabilities.clipboard == true && session.inputReady
        model.displays = host?.displayList ?? []
        model.touchMode = stage.touchMode
        model.codec = session.codecPreference
        model.maxFps = session.maxFps
        model.maxBitrateKbps = session.maxBitrateKbps
    }

    func connect(_ input: String) {
        do {
            #if DEBUG && targetEnvironment(simulator)
            let endpoint = try HostEndpoint(input, allowSimulatorLoopback: true)
            #else
            let endpoint = try HostEndpoint(input)
            #endif
            let address = endpoint.base.absoluteString
            UserDefaults.standard.set(address, forKey: "host")
            model.recents = Array(([address] + model.recents.filter { $0 != address }).prefix(5))
            UserDefaults.standard.set(model.recents, forKey: "recentHosts")
            session.start(endpoint)
        } catch {
            show(error.localizedDescription)
        }
    }

    func forget(_ address: String) {
        model.recents.removeAll { $0 == address }
        UserDefaults.standard.set(model.recents, forKey: "recentHosts")
    }

    /// Uses any reachable nanoctl agent as a tailnet directory. Moonlight makes
    /// host selection feel instant because hosts are already visible; recents
    /// are the bootstrap seeds here and every directory query runs in parallel.
    func refreshHosts(extraSeed: String? = nil) {
        discoveryTask?.cancel()
        var seeds = model.recents
        if let extraSeed {
            let trimmed = extraSeed.trimmingCharacters(in: .whitespacesAndNewlines)
            if !trimmed.isEmpty, !seeds.contains(trimmed) { seeds.insert(trimmed, at: 0) }
        }
        guard !seeds.isEmpty else {
            model.discoveredHosts = []
            model.discoveringHosts = false
            model.discoveryNotice = nil
            return
        }
        model.discoveringHosts = true
        model.discoveryNotice = nil
        discoveryTask = Task { [weak self] in
            var found: [String: DiscoveredHost] = [:]
            var failures = 0
            await withTaskGroup(of: Result<[DiscoveredHost], Error>.self) { group in
                for seed in seeds {
                    group.addTask {
                        do {
                            return .success(try await Self.fetchDirectory(seed))
                        } catch {
                            return .failure(error)
                        }
                    }
                }
                for await result in group {
                    switch result {
                    case .success(let hosts):
                        for host in hosts where !host.url.isEmpty {
                            if var existing = found[host.url] {
                                if !existing.online, host.online { existing = host }
                                found[host.url] = existing
                            } else {
                                found[host.url] = host
                            }
                        }
                    case .failure:
                        failures += 1
                    }
                }
            }
            guard !Task.isCancelled, let self else { return }
            let sorted = found.values.sorted {
                if $0.online != $1.online { return $0.online && !$1.online }
                return $0.displayName.localizedCaseInsensitiveCompare($1.displayName) == .orderedAscending
            }
            model.discoveredHosts = sorted
            model.discoveringHosts = false
            if sorted.isEmpty {
                model.discoveryNotice = failures == 0 ? "No nanoctl hosts found" : "Host discovery unavailable"
            } else if failures > 0 {
                model.discoveryNotice = "\(failures) director\(failures == 1 ? "y" : "ies") unavailable"
            } else {
                model.discoveryNotice = nil
            }
        }
    }

    private nonisolated static func fetchDirectory(_ seed: String) async throws -> [DiscoveredHost] {
        #if DEBUG && targetEnvironment(simulator)
        let endpoint = try HostEndpoint(seed, allowSimulatorLoopback: true)
        #else
        let endpoint = try HostEndpoint(seed)
        #endif
        var request = URLRequest(url: endpoint.base.appending(path: "v1/hosts"))
        request.timeoutInterval = 5
        let (data, response) = try await URLSession.shared.data(for: request)
        guard let http = response as? HTTPURLResponse, http.statusCode == 200 else {
            throw ViewerError("Host discovery failed")
        }
        let directory = try JSONDecoder().decode(HostDirectory.self, from: data)
        return ([directory.selfHost] + directory.hosts).map { host in
            var host = host
            host.source = seed
            return host
        }
    }

    func setTouchMode(_ mode: TouchMode) {
        stage.touchMode = mode
        update()
    }

    func setKeyboardVisible(_ visible: Bool) {
        stage.setKeyboardVisible(visible)
    }

    func restoreInputFocus() {
        stage.restoreInputFocus()
    }

    func setCodec(_ codec: CodecPreference) {
        guard codec != session.codecPreference else { return }
        session.codecPreference = codec
        if session.wantsConnection { session.restart() }
        update()
    }

    func show(_ message: String) {
        toastTask?.cancel()
        model.toast = message
        toastTask = Task { [weak self] in
            try? await Task.sleep(for: .seconds(4))
            guard !Task.isCancelled else { return }
            self?.model.toast = nil
        }
    }

    /// AirPlay from the route picker carries audio only; video needs iOS to
    /// expose the TV as a second screen, which Screen Mirroring does.
    private func checkAirPlay() {
        airPlayCheck?.cancel()
        guard let tv = AVAudioSession.sharedInstance().currentRoute.outputs
            .first(where: { $0.portType == .airPlay }) else { return }
        airPlayCheck = Task { [weak self] in
            try? await Task.sleep(for: .seconds(2))
            guard !Task.isCancelled, let self, self.external == nil else { return }
            self.show("Sound is playing on \(tv.portName). For video, open Control Center › Screen Mirroring.")
        }
    }
}

@MainActor
private var desktop: Desktop { (UIApplication.shared.delegate as! App).desktop }

final class PhoneScene: UIResponder, UIWindowSceneDelegate {
    var window: UIWindow?
    private var resume = false

    func scene(_ scene: UIScene, willConnectTo session: UISceneSession, options: UIScene.ConnectionOptions) {
        guard let scene = scene as? UIWindowScene else { return }
        desktop.gamepad.onChange = {
            if desktop.session.wantsConnection { desktop.show(desktop.gamepad.status) }
        }
        let window = UIWindow(windowScene: scene)
        window.rootViewController = UIHostingController(rootView: RootView(desktop: desktop))
        window.makeKeyAndVisible()
        self.window = window
        desktop.update()
        #if DEBUG && targetEnvironment(simulator)
        if let address = ProcessInfo.processInfo.environment["NANOCTL_BENCHMARK_URL"] {
            let probe = LatencyProbe(session: desktop.session)
            desktop.probe = probe
            desktop.stage.video.probe = probe
            desktop.connect(address)
        }
        #endif
    }

    func sceneDidBecomeActive(_ scene: UIScene) { desktop.gamepad.setActive(true) }
    func sceneWillResignActive(_ scene: UIScene) { desktop.gamepad.setActive(false) }

    func sceneDidEnterBackground(_ scene: UIScene) {
        resume = desktop.session.wantsConnection
        desktop.session.stop()
    }

    func sceneWillEnterForeground(_ scene: UIScene) {
        if resume { desktop.session.restart() }
        resume = false
    }
}

final class ExternalScene: UIResponder, UIWindowSceneDelegate {
    var window: UIWindow?

    func scene(_ scene: UIScene, willConnectTo session: UISceneSession, options: UIScene.ConnectionOptions) {
        guard let scene = scene as? UIWindowScene else { return }
        let controller = UIViewController()
        let video = VideoView()
        controller.view = video
        let window = UIWindow(windowScene: scene)
        // TVs overscan (crop the edges); scaling keeps the full frame visible.
        window.screen.overscanCompensation = .scale
        window.rootViewController = controller
        window.isHidden = false
        self.window = window
        desktop.external = video
        desktop.update()
    }

    func sceneDidDisconnect(_ scene: UIScene) {
        let video = window?.rootViewController?.view as? VideoView
        video?.show(nil)
        if desktop.external === video {
            desktop.external = nil
            desktop.update()
        }
        window = nil
    }
}
