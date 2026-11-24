import UIKit
import WebRTC

@main
@MainActor
final class App: UIResponder, UIApplicationDelegate {
    let desktop = Desktop()
}

@MainActor
final class Desktop {
    let session = Session()
    let mailbox = FrameMailbox()
    lazy var gamepad = Gamepad(session: session)
    weak var phone: PhoneViewController?
    weak var external: VideoView?
    private var track: RTCVideoTrack?
    #if DEBUG && targetEnvironment(simulator)
    var probe: LatencyProbe?
    #endif

    init() {
        session.onChange = { [weak self] in self?.update() }
    }

    var hasVideo: Bool { session.track != nil || session.transport == .dataChannel }

    func update() {
        if track !== session.track {
            track?.remove(mailbox)
            mailbox.renderFrame(nil)
            track = session.track
            track?.add(mailbox)
        }
        phone?.update(external: external != nil)
        let pixels = session.transport == .dataChannel ? session.pixelMailbox : nil
        external?.show(track == nil ? nil : mailbox)
        external?.show(pixels: pixels)
    }
}

@MainActor
private var desktop: Desktop { (UIApplication.shared.delegate as! App).desktop }

final class PhoneScene: UIResponder, UIWindowSceneDelegate {
    var window: UIWindow?

    func scene(_ scene: UIScene, willConnectTo session: UISceneSession, options: UIScene.ConnectionOptions) {
        guard let scene = scene as? UIWindowScene else { return }
        let phone = PhoneViewController(desktop: desktop)
        desktop.phone = phone
        desktop.gamepad.onChange = { [weak phone] in phone?.refreshGamepad() }
        let window = UIWindow(windowScene: scene)
        window.rootViewController = phone
        window.makeKeyAndVisible()
        self.window = window
        desktop.update()
        #if DEBUG && targetEnvironment(simulator)
        if let address = ProcessInfo.processInfo.environment["NANOCTL_BENCHMARK_URL"] {
            let probe = LatencyProbe(session: desktop.session)
            desktop.probe = probe
            phone.video.probe = probe
            phone.connect(address)
        }
        #endif
    }

    func sceneDidBecomeActive(_ scene: UIScene) { desktop.gamepad.setActive(true) }
    func sceneWillResignActive(_ scene: UIScene) { desktop.gamepad.setActive(false) }

    func sceneDidEnterBackground(_ scene: UIScene) { desktop.session.stop() }
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

final class PhoneViewController: UIViewController, UITextFieldDelegate {
    let video = VideoView()
    private let desktop: Desktop
    private let address = UITextField()
    private let state = UILabel()
    private let gamepad = UILabel()
    private let output = UILabel()
    private let stats = UILabel()
    private let toggle = UIButton(type: .system)
    private let space = UIButton(type: .system)
    private let codec = UISegmentedControl()
    private let audio = UIButton(type: .system)
    private let displays = UIButton(type: .system)
    private let paste = UIButton(type: .system)
    private let surface = TouchSurface()
    private let cursor = UIView()
    private let touchModeControl = UISegmentedControl(items: TouchMode.allCases.map(\.label))
    private lazy var gestures = TouchGestures(
        mode: TouchMode(rawValue: UserDefaults.standard.string(forKey: "touchMode") ?? "") ?? .trackpad,
        geometry: { [weak self] in self?.inputGeometry() },
        send: { [weak self] event in
            self?.desktop.session.sendPointer(x: event.x, y: event.y, action: event.action, button: event.button,
                                              deltaX: event.deltaX, deltaY: event.deltaY)
        })
    private let screen = UIView()
    private let fullBtn = UIButton(type: .system)
    private let exitFull = UIButton(type: .system)
    private var stack: UIStackView!
    private var usingExternal = false
    private var fullscreen = false

    override var prefersStatusBarHidden: Bool { fullscreen }

    init(desktop: Desktop) {
        self.desktop = desktop
        super.init(nibName: nil, bundle: nil)
    }

    required init?(coder: NSCoder) { nil }

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .systemBackground
        let title = UILabel()
        title.text = "nanoctl"
        title.font = .preferredFont(forTextStyle: .largeTitle)
        title.accessibilityTraits.insert(.header)
        address.placeholder = "host.tailnet.ts.net:7337"
        address.keyboardType = .URL
        address.autocapitalizationType = .none
        address.autocorrectionType = .no
        address.returnKeyType = .go
        address.clearButtonMode = .whileEditing
        address.borderStyle = .roundedRect
        address.delegate = self
        address.accessibilityLabel = "Host address"
        address.text = UserDefaults.standard.string(forKey: "host")

        toggle.configuration = .filled()
        toggle.setTitle("Connect", for: .normal)
        toggle.addTarget(self, action: #selector(toggleConnection), for: .touchUpInside)
        space.configuration = .tinted()
        space.setTitle("Space", for: .normal)
        space.addTarget(self, action: #selector(sendSpace), for: .touchUpInside)

        for label in [state, gamepad, output, stats] {
            label.font = .preferredFont(forTextStyle: .footnote)
            label.textColor = .secondaryLabel
            label.numberOfLines = 0
        }
        output.textAlignment = .center
        stats.textColor = .white
        stats.backgroundColor = UIColor.black.withAlphaComponent(0.5)
        stats.font = .monospacedDigitSystemFont(ofSize: 11, weight: .regular)

        for (index, preference) in CodecPreference.allCases.enumerated() {
            codec.insertSegment(withTitle: preference.label, at: index, animated: false)
        }
        codec.selectedSegmentIndex = CodecPreference.allCases.firstIndex(of: desktop.session.codecPreference) ?? 0
        codec.addTarget(self, action: #selector(codecChanged), for: .valueChanged)
        codec.accessibilityLabel = "Video codec"
        codec.apportionsSegmentWidthsByContent = true
        codec.setContentCompressionResistancePriority(.required, for: .horizontal)
        codec.setContentHuggingPriority(.required, for: .horizontal)

        audio.configuration = .tinted()
        audio.setTitle("Audio", for: .normal)
        audio.addTarget(self, action: #selector(toggleAudio), for: .touchUpInside)
        displays.configuration = .tinted()
        displays.setTitle("Display", for: .normal)
        displays.addTarget(self, action: #selector(pickDisplay), for: .touchUpInside)
        paste.configuration = .tinted()
        paste.setTitle("Paste→host", for: .normal)
        paste.addTarget(self, action: #selector(sendPaste), for: .touchUpInside)

        fullBtn.configuration = .tinted()
        fullBtn.setTitle("Full", for: .normal)
        fullBtn.addTarget(self, action: #selector(toggleFullscreen), for: .touchUpInside)
        exitFull.configuration = .tinted()
        exitFull.setTitle("\u{2715}", for: .normal)
        exitFull.isHidden = true
        exitFull.addTarget(self, action: #selector(toggleFullscreen), for: .touchUpInside)
        view.addSubview(exitFull)
        exitFull.translatesAutoresizingMaskIntoConstraints = false
        NSLayoutConstraint.activate([
            exitFull.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor, constant: 8),
            exitFull.trailingAnchor.constraint(equalTo: view.safeAreaLayoutGuide.trailingAnchor, constant: -8),
        ])

        let buttons = UIStackView(arrangedSubviews: [audio, displays, paste, fullBtn])
        buttons.spacing = 8
        buttons.distribution = .fillEqually
        touchModeControl.selectedSegmentIndex = gestures.mode == .direct ? 1 : 0
        touchModeControl.addTarget(self, action: #selector(touchModeChanged), for: .valueChanged)
        touchModeControl.accessibilityLabel = "Touch mode"
        let controls = UIStackView(arrangedSubviews: [codec, touchModeControl, buttons])
        controls.axis = .vertical
        controls.spacing = 8

        let row = UIStackView(arrangedSubviews: [address, toggle])
        row.spacing = 8
        toggle.setContentHuggingPriority(.required, for: .horizontal)
        toggle.setContentCompressionResistancePriority(.required, for: .horizontal)

        screen.backgroundColor = .secondarySystemBackground
        screen.clipsToBounds = true
        screen.layer.cornerRadius = 12
        for overlay in [video, output, surface] {
            overlay.translatesAutoresizingMaskIntoConstraints = false
            screen.addSubview(overlay)
        }
        surface.gestures = gestures
        gestures.onCursor = { [weak self] _ in self?.placeCursor() }
        gestures.onZoom = { [weak self] zoom in
            self?.video.transform = CGAffineTransform(translationX: zoom.offsetX, y: zoom.offsetY)
                .scaledBy(x: zoom.scale, y: zoom.scale)
            self?.placeCursor()
        }
        cursor.isUserInteractionEnabled = false
        cursor.bounds = CGRect(x: 0, y: 0, width: 16, height: 16)
        cursor.layer.cornerRadius = 8
        cursor.layer.borderWidth = 2
        cursor.layer.borderColor = UIColor.white.cgColor
        cursor.backgroundColor = UIColor(red: 0.06, green: 0.09, blue: 0.13, alpha: 0.6)
        screen.addSubview(cursor)
        NSLayoutConstraint.activate([video, surface].flatMap { overlay in
            [overlay.leadingAnchor.constraint(equalTo: screen.leadingAnchor),
             overlay.trailingAnchor.constraint(equalTo: screen.trailingAnchor),
             overlay.topAnchor.constraint(equalTo: screen.topAnchor),
             overlay.bottomAnchor.constraint(equalTo: screen.bottomAnchor)]
        } + [output.centerXAnchor.constraint(equalTo: screen.centerXAnchor),
             output.centerYAnchor.constraint(equalTo: screen.centerYAnchor),
             output.leadingAnchor.constraint(greaterThanOrEqualTo: screen.leadingAnchor, constant: 8),
             screen.heightAnchor.constraint(greaterThanOrEqualToConstant: 48)])

        stats.translatesAutoresizingMaskIntoConstraints = false
        screen.addSubview(stats)
        NSLayoutConstraint.activate([
            stats.leadingAnchor.constraint(equalTo: screen.leadingAnchor, constant: 8),
            stats.topAnchor.constraint(equalTo: screen.topAnchor, constant: 8),
        ])

        stack = UIStackView(arrangedSubviews: [title, row, state, screen, controls, gamepad, space])
        stack.axis = .vertical
        stack.spacing = 8
        stack.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(stack)
        let safe = view.safeAreaLayoutGuide
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: safe.leadingAnchor, constant: 16),
            stack.trailingAnchor.constraint(equalTo: safe.trailingAnchor, constant: -16),
            stack.topAnchor.constraint(equalTo: safe.topAnchor, constant: 8),
            stack.bottomAnchor.constraint(equalTo: safe.bottomAnchor, constant: -8),
        ])
    }

    func update(external: Bool) {
        loadViewIfNeeded()
        usingExternal = external
        let session = desktop.session
        let hasVideo = desktop.hasVideo
        state.text = session.state
        stats.text = session.statsLine
        stats.isHidden = session.statsLine.isEmpty
        let active = session.state == "Connecting…" || session.state.hasPrefix("Live ·")
        toggle.setTitle(active ? "Disconnect" : "Connect", for: .normal)
        address.isEnabled = !active
        codec.isEnabled = !active
        let audioAvailable = session.host?.audioAvailable == true
        audio.isEnabled = audioAvailable
        audio.setTitle(audioAvailable ? (session.audioEnabled ? "Audio: on" : "Audio: off") : "Audio: n/a", for: .normal)
        let list = session.host?.displayList ?? []
        displays.isHidden = list.count < 2
        paste.isEnabled = session.host?.capabilities.clipboard == true
        space.isEnabled = session.inputReady
        surface.isUserInteractionEnabled = session.inputReady
        if !session.inputReady { gestures.reset() }
        if external { gestures.onZoom?(Zoom()); gestures.zoom = Zoom() }
        video.isHidden = external || !hasVideo
        UIApplication.shared.isIdleTimerDisabled = hasVideo
        video.show(hasVideo && !external && session.transport == .rtp ? desktop.mailbox : nil)
        video.show(pixels: hasVideo && !external && session.transport == .dataChannel ? session.pixelMailbox : nil)
        output.isHidden = hasVideo && !external
        output.text = external ? "Video on the external display\nUse this screen as a trackpad" : "Remote video"
        placeCursor()
        refreshGamepad()
    }

    func refreshGamepad() {
        gamepad.text = desktop.gamepad.status
    }

    func textFieldShouldReturn(_ textField: UITextField) -> Bool {
        connect(address.text ?? "")
        return true
    }

    func connect(_ input: String) {
        address.resignFirstResponder()
        do {
            #if DEBUG && targetEnvironment(simulator)
            let endpoint = try HostEndpoint(input, allowSimulatorLoopback: true)
            #else
            let endpoint = try HostEndpoint(input)
            #endif
            address.text = endpoint.base.absoluteString
            UserDefaults.standard.set(address.text, forKey: "host")
            desktop.session.start(endpoint)
        } catch {
            state.text = error.localizedDescription
        }
    }

    @objc private func toggleConnection() {
        if address.isEnabled { connect(address.text ?? "") }
        else { desktop.session.stop() }
    }

    @objc private func sendSpace() {
        if desktop.session.sendKey(true) { desktop.session.sendKey(false) }
    }

    @objc private func codecChanged() {
        let index = codec.selectedSegmentIndex
        guard index >= 0 else { return }
        desktop.session.codecPreference = CodecPreference.allCases[index]
    }

    @objc private func toggleFullscreen() {
        fullscreen = !fullscreen
        screen.removeFromSuperview()
        if fullscreen {
            // Raw fullscreen: video edge-to-edge, everything else hidden.
            view.insertSubview(screen, belowSubview: exitFull)
            screen.translatesAutoresizingMaskIntoConstraints = false
            NSLayoutConstraint.activate([
                screen.leadingAnchor.constraint(equalTo: view.leadingAnchor),
                screen.trailingAnchor.constraint(equalTo: view.trailingAnchor),
                screen.topAnchor.constraint(equalTo: view.topAnchor),
                screen.bottomAnchor.constraint(equalTo: view.bottomAnchor),
            ])
        } else {
            stack.insertArrangedSubview(screen, at: 3)
        }
        stack.isHidden = fullscreen
        exitFull.isHidden = !fullscreen
        setNeedsStatusBarAppearanceUpdate()
    }

    @objc private func toggleAudio() {
        desktop.session.audioEnabled = !desktop.session.audioEnabled
        update(external: usingExternal)
    }

    @objc private func sendPaste() {
        _ = desktop.session.sendClipboard()
    }

    @objc private func pickDisplay() {
        let list = desktop.session.host?.displayList ?? []
        guard !list.isEmpty else { return }
        let sheet = UIAlertController(title: "Display", message: nil, preferredStyle: .actionSheet)
        for display in list {
            sheet.addAction(UIAlertAction(title: "\(display.name) (\(display.width)×\(display.height))",
                                          style: .default) { [weak self] _ in
                _ = self?.desktop.session.selectDisplay(display.id)
            })
        }
        sheet.addAction(UIAlertAction(title: "Cancel", style: .cancel))
        sheet.popoverPresentationController?.sourceView = displays
        present(sheet, animated: true)
    }

    /// The letterboxed video rect in surface coordinates, including zoom.
    private func inputGeometry() -> CGRect? {
        let area = usingExternal ? surface.bounds : video.convert(video.inputArea, to: surface)
        return area.isEmpty ? nil : area
    }

    private func placeCursor() {
        cursor.isHidden = gestures.mode != .trackpad || usingExternal || !desktop.hasVideo
            || !desktop.session.inputReady
        guard !cursor.isHidden, let area = inputGeometry() else { return }
        cursor.center = surface.convert(CGPoint(x: area.minX + gestures.cursor.x * area.width,
                                                y: area.minY + gestures.cursor.y * area.height), to: screen)
    }

    override func viewDidLayoutSubviews() {
        super.viewDidLayoutSubviews()
        placeCursor()
    }

    @objc private func touchModeChanged() {
        gestures.reset()
        gestures.mode = touchModeControl.selectedSegmentIndex == 1 ? .direct : .trackpad
        UserDefaults.standard.set(gestures.mode.rawValue, forKey: "touchMode")
        placeCursor()
    }
}

/// Raw multi-touch feed for `TouchGestures`; UIKit recognizers would delay
/// or swallow the taps and holds the trackpad needs.
final class TouchSurface: UIView {
    var gestures: TouchGestures?

    init() {
        super.init(frame: .zero)
        isMultipleTouchEnabled = true
    }

    required init?(coder: NSCoder) { nil }

    private func samples(_ touches: Set<UITouch>) -> [TouchSample] {
        touches.map { touch in
            let point = touch.location(in: self)
            return TouchSample(id: ObjectIdentifier(touch).hashValue, x: point.x, y: point.y, t: touch.timestamp * 1000)
        }
    }

    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) {
        samples(touches).forEach { gestures?.down($0) }
    }

    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
        samples(touches).forEach { gestures?.move($0) }
    }

    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) {
        samples(touches).forEach { gestures?.up($0) }
    }

    override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) {
        gestures?.cancel()
    }
}
