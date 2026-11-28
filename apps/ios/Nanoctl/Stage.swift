import UIKit

/// Edge-to-edge video plus the raw touch surface and trackpad cursor. Owned
/// by `Desktop` so SwiftUI re-renders never recreate the Metal view.
@MainActor
final class Stage: UIView {
    let video = VideoView()
    private let surface = TouchSurface()
    private let cursor = UIView()
    private weak var desktop: Desktop?
    private var external = false
    private var inputWasReady = false
    private var touchModeBeforeExternal: TouchMode?
    private(set) lazy var keyboardForwarder = KeyboardForwarder(
        sendKey: { [weak self] down, key, repeating in
            self?.desktop?.session.sendKey(down, key: key, repeating: repeating) ?? false
        },
        sendText: { [weak self] text in
            self?.desktop?.session.sendText(text) ?? false
        })
    private lazy var keyboardInput = KeyboardInputView(forwarder: keyboardForwarder)
    var onKeyboardVisible: ((Bool) -> Void)?
    private(set) lazy var gestures = TouchGestures(
        mode: TouchMode(rawValue: UserDefaults.standard.string(forKey: "touchMode") ?? "") ?? .trackpad,
        geometry: { [weak self] in self?.inputGeometry() },
        send: { [weak self] event in
            self?.desktop?.session.sendPointer(x: event.x, y: event.y, action: event.action, button: event.button,
                                               deltaX: event.deltaX, deltaY: event.deltaY)
        },
        pointerSensitivity: Self.inputScale(forKey: "inputPointerSensitivity"),
        scrollSpeed: Self.inputScale(forKey: "inputScrollSpeed"))

    private static func inputScale(forKey key: String) -> Double {
        let value = UserDefaults.standard.double(forKey: key)
        return value > 0 ? value : 1
    }

    var pointerSensitivity: Double {
        get { gestures.pointerSensitivity }
        set {
            gestures.pointerSensitivity = newValue
            UserDefaults.standard.set(newValue, forKey: "inputPointerSensitivity")
        }
    }

    var scrollSpeed: Double {
        get { gestures.scrollSpeed }
        set {
            gestures.scrollSpeed = newValue
            UserDefaults.standard.set(newValue, forKey: "inputScrollSpeed")
        }
    }

    var touchMode: TouchMode {
        get { gestures.mode }
        set {
            let effective = external ? .trackpad : newValue
            touchModeBeforeExternal = external && newValue == .direct ? .direct : nil
            gestures.reset()
            gestures.mode = effective
            UserDefaults.standard.set(newValue.rawValue, forKey: "touchMode")
            placeCursor()
        }
    }

    init(desktop: Desktop) {
        self.desktop = desktop
        super.init(frame: .zero)
        backgroundColor = .black
        clipsToBounds = true
        for child in [video, surface] {
            child.frame = bounds
            child.autoresizingMask = [.flexibleWidth, .flexibleHeight]
            addSubview(child)
        }
        keyboardInput.center = CGPoint(x: 0.5, y: 0.5)
        addSubview(keyboardInput)
        NotificationCenter.default.addObserver(forName: UIResponder.keyboardDidHideNotification, object: nil,
                                               queue: .main) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self, !self.keyboardInput.isFirstResponder else { return }
                self.onKeyboardVisible?(false)
            }
        }
        surface.gestures = gestures
        gestures.onCursor = { [weak self] _ in self?.placeCursor() }
        gestures.onContact = { [weak self] contacting in self?.setCursorContact(contacting) }
        gestures.onZoom = { [weak self] zoom in
            self?.video.transform = CGAffineTransform(translationX: zoom.offsetX, y: zoom.offsetY)
                .scaledBy(x: zoom.scale, y: zoom.scale)
            self?.placeCursor()
            self?.publishViewportSize()
        }
        cursor.isUserInteractionEnabled = false
        cursor.bounds = CGRect(x: 0, y: 0, width: 20, height: 20)
        cursor.layer.cornerRadius = 10
        cursor.layer.borderWidth = 1.5
        cursor.layer.borderColor = UIColor.white.withAlphaComponent(0.94).cgColor
        cursor.backgroundColor = UIColor.black.withAlphaComponent(0.3)
        cursor.layer.shadowColor = UIColor.black.cgColor
        cursor.layer.shadowOpacity = 0.42
        cursor.layer.shadowRadius = 5
        cursor.layer.shadowOffset = .zero
        let dot = UIView(frame: CGRect(x: 8, y: 8, width: 4, height: 4))
        dot.layer.cornerRadius = 2
        dot.backgroundColor = .white
        dot.autoresizingMask = [.flexibleLeftMargin, .flexibleRightMargin, .flexibleTopMargin, .flexibleBottomMargin]
        cursor.addSubview(dot)
        addSubview(cursor)
    }

    required init?(coder: NSCoder) { nil }

    func update(external: Bool) {
        guard let desktop else { return }
        self.external = external
        if external, gestures.mode == .direct {
            touchModeBeforeExternal = .direct
            gestures.reset()
            gestures.mode = .trackpad
        } else if !external, let restore = touchModeBeforeExternal {
            touchModeBeforeExternal = nil
            if UserDefaults.standard.string(forKey: "touchMode") == restore.rawValue {
                gestures.reset()
                gestures.mode = restore
            }
        }
        let session = desktop.session
        let hasVideo = desktop.hasVideo
        surface.isUserInteractionEnabled = session.inputReady
        if !session.inputReady {
            gestures.reset()
            keyboardForwarder.releaseAll()
            setKeyboardVisible(false)
        }
        if session.inputReady, !inputWasReady, !keyboardInput.isFirstResponder {
            keyboardInput.becomeFirstResponder()
        }
        inputWasReady = session.inputReady
        if external, gestures.zoom != Zoom() {
            gestures.zoom = Zoom()
            gestures.onZoom?(Zoom())
        }
        video.isHidden = external || !hasVideo
        video.show(hasVideo && !external && session.transport == .rtp ? desktop.mailbox : nil)
        video.show(pixels: hasVideo && !external && session.transport == .dataChannel ? session.pixelMailbox : nil)
        placeCursor()
        publishViewportSize()
    }

    override var canBecomeFirstResponder: Bool { true }

    func restoreInputFocus() {
        guard desktop?.session.inputReady == true, !keyboardInput.isFirstResponder else { return }
        keyboardInput.becomeFirstResponder()
    }

    func setKeyboardVisible(_ visible: Bool) {
        keyboardInput.wantsSoftwareKeyboard = visible
        if visible {
            guard desktop?.session.inputReady == true else { return }
            if !keyboardInput.isFirstResponder { keyboardInput.becomeFirstResponder() }
            keyboardInput.reloadInputViews()
            if keyboardInput.isFirstResponder { onKeyboardVisible?(true) }
        } else {
            keyboardInput.reloadInputViews()
            keyboardForwarder.releaseAll()
            onKeyboardVisible?(false)
        }
    }

    override func pressesBegan(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        forwardKeyPresses(presses, with: event, began: true)
    }

    override func pressesEnded(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        forwardKeyPresses(presses, with: event, began: false)
    }

    override func pressesCancelled(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        forwardKeyPresses(presses, with: event, began: false)
    }

    private func forwardKeyPresses(_ presses: Set<UIPress>, with event: UIPressesEvent?, began: Bool) {
        guard desktop?.session.inputReady == true else {
            began ? super.pressesBegan(presses, with: event) : super.pressesEnded(presses, with: event)
            return
        }
        var remaining = Set<UIPress>()
        for press in presses {
            guard let key = press.key, keyboardForwarder.press(key, down: began) else {
                remaining.insert(press)
                continue
            }
        }
        guard !remaining.isEmpty else { return }
        began ? super.pressesBegan(remaining, with: event) : super.pressesEnded(remaining, with: event)
    }

    /// The letterboxed video rect in surface coordinates, including zoom.
    private func inputGeometry() -> CGRect? {
        let area = external ? surface.bounds : video.convert(video.inputArea, to: surface)
        return area.isEmpty ? nil : area
    }

    private func setCursorContact(_ contacting: Bool) {
        UIView.animate(withDuration: 0.16, delay: 0, options: [.beginFromCurrentState, .curveEaseOut]) {
            self.cursor.transform = contacting ? CGAffineTransform(scaleX: 0.78, y: 0.78) : .identity
            self.cursor.backgroundColor = contacting
                ? UIColor.white.withAlphaComponent(0.22)
                : UIColor.black.withAlphaComponent(0.3)
        }
    }

    private func placeCursor() {
        cursor.isHidden = gestures.mode != .trackpad || external || desktop?.hasVideo != true
            || desktop?.session.inputReady != true
        guard !cursor.isHidden, let area = inputGeometry() else { return }
        cursor.center = surface.convert(CGPoint(x: area.minX + gestures.cursor.x * area.width,
                                                y: area.minY + gestures.cursor.y * area.height), to: self)
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        placeCursor()
        publishViewportSize()
    }

    private func publishViewportSize() {
        guard let desktop else { return }
        // Zooming and external output can expose detail outside the nominal
        // letterboxed viewport, so they must not ask the host to downscale.
        guard !external, gestures.zoom.scale <= 1.01 else {
            desktop.session.setViewportSize(width: nil, height: nil)
            return
        }
        let area = video.inputArea
        guard !area.isEmpty else {
            desktop.session.setViewportSize(width: nil, height: nil)
            return
        }
        let scale = window?.windowScene?.screen.scale ?? traitCollection.displayScale
        func evenPhysicalPixels(_ points: CGFloat) -> Int {
            max(2, Int((points * scale / 2).rounded(.up)) * 2)
        }
        desktop.session.setViewportSize(
            width: evenPhysicalPixels(area.width),
            height: evenPhysicalPixels(area.height)
        )
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
        let touchSamples = samples(touches)
        touchSamples.forEach { gestures?.down($0) }
        if gestures?.mode == .direct {
            touchSamples.forEach { ripple(at: CGPoint(x: $0.x, y: $0.y)) }
        }
    }

    private func ripple(at point: CGPoint) {
        let size: CGFloat = 24
        let view = UIView(frame: CGRect(x: point.x - size / 2, y: point.y - size / 2, width: size, height: size))
        view.isUserInteractionEnabled = false
        view.layer.cornerRadius = size / 2
        view.layer.borderWidth = 1.5
        view.layer.borderColor = UIColor.white.withAlphaComponent(0.32).cgColor
        view.backgroundColor = UIColor.white.withAlphaComponent(0.08)
        addSubview(view)
        UIView.animate(withDuration: 0.28, delay: 0, options: [.curveEaseOut]) {
            view.transform = CGAffineTransform(scaleX: 1.75, y: 1.75)
            view.alpha = 0
        } completion: { _ in
            view.removeFromSuperview()
        }
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
