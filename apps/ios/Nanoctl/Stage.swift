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
    private(set) lazy var gestures = TouchGestures(
        mode: TouchMode(rawValue: UserDefaults.standard.string(forKey: "touchMode") ?? "") ?? .trackpad,
        geometry: { [weak self] in self?.inputGeometry() },
        send: { [weak self] event in
            self?.desktop?.session.sendPointer(x: event.x, y: event.y, action: event.action, button: event.button,
                                               deltaX: event.deltaX, deltaY: event.deltaY)
        })

    var touchMode: TouchMode {
        get { gestures.mode }
        set {
            gestures.reset()
            gestures.mode = newValue
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
        cursor.layer.shadowOpacity = 0.4
        cursor.layer.shadowRadius = 2
        cursor.layer.shadowOffset = .zero
        addSubview(cursor)
    }

    required init?(coder: NSCoder) { nil }

    func update(external: Bool) {
        guard let desktop else { return }
        self.external = external
        let session = desktop.session
        let hasVideo = desktop.hasVideo
        surface.isUserInteractionEnabled = session.inputReady
        if !session.inputReady { gestures.reset() }
        if external, gestures.zoom != Zoom() {
            gestures.zoom = Zoom()
            gestures.onZoom?(Zoom())
        }
        video.isHidden = external || !hasVideo
        video.show(hasVideo && !external && session.transport == .rtp ? desktop.mailbox : nil)
        video.show(pixels: hasVideo && !external && session.transport == .dataChannel ? session.pixelMailbox : nil)
        placeCursor()
    }

    /// The letterboxed video rect in surface coordinates, including zoom.
    private func inputGeometry() -> CGRect? {
        let area = external ? surface.bounds : video.convert(video.inputArea, to: surface)
        return area.isEmpty ? nil : area
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
