import CoreGraphics
import Foundation

/// Trackpad: slide moves the cursor relatively, tap clicks, tap twice and hold
/// drags. Direct: tap where you want to click, touch and hold to right-click.
/// Both: two fingers scroll or pinch-zoom, two/three-finger taps right/middle
/// click, a double two-finger tap resets zoom. Mirrors the web viewer's
/// `touch-input.ts` so every nanoctl viewer feels the same.
enum TouchMode: String, CaseIterable {
    case trackpad
    case direct

    var label: String { self == .trackpad ? "Trackpad" : "Direct touch" }
}

struct TouchSample {
    let id: Int
    let x: CGFloat
    let y: CGFloat
    /// Milliseconds.
    let t: Double
}

struct Zoom: Equatable {
    var scale: CGFloat = 1
    var offsetX: CGFloat = 0
    var offsetY: CGFloat = 0
}

struct PointerEvent: Equatable {
    let action: String
    let x: Double
    let y: Double
    var button = 0
    var deltaX: Double = 0
    var deltaY: Double = 0
}

@MainActor
final class TouchGestures {
    typealias Schedule = (_ milliseconds: Double, _ fire: @escaping @MainActor () -> Void) -> () -> Void

    var mode: TouchMode
    var pointerSensitivity: Double
    var scrollSpeed: Double
    var geometry: () -> CGRect?
    var send: (PointerEvent) -> Void
    var onCursor: ((CGPoint) -> Void)?
    var onZoom: ((Zoom) -> Void)?
    var cursor = CGPoint(x: 0.5, y: 0.5)
    var zoom = Zoom()

    private struct Finger { var start: TouchSample; var last: TouchSample }
    private enum Decision { case pending, scroll, pinch }
    private struct Pair { var center: CGPoint; var distance: CGFloat; var zoom: Zoom }

    private let schedule: Schedule
    private var fingers: [Int: Finger] = [:]
    private var order: [Int] = []
    private var count = 0
    private var started = 0.0
    private var moved = false
    private var consumed = false
    private var held = false
    private var lastTap = -Double.infinity
    private var lastTwoTap = -Double.infinity
    private var cancelTimer: (() -> Void)?
    private var decision = Decision.pending
    private var pair: Pair?
    private var scroll = CGPoint.zero

    init(mode: TouchMode, geometry: @escaping () -> CGRect?, send: @escaping (PointerEvent) -> Void,
         pointerSensitivity: Double = 1, scrollSpeed: Double = 1,
         schedule: @escaping Schedule = TouchGestures.dispatchSchedule) {
        self.mode = mode
        self.pointerSensitivity = pointerSensitivity
        self.scrollSpeed = scrollSpeed
        self.geometry = geometry
        self.send = send
        self.schedule = schedule
    }

    nonisolated static func dispatchSchedule(_ milliseconds: Double, _ fire: @escaping @MainActor () -> Void) -> () -> Void {
        let work = DispatchWorkItem { MainActor.assumeIsolated { fire() } }
        DispatchQueue.main.asyncAfter(deadline: .now() + milliseconds / 1000, execute: work)
        return { work.cancel() }
    }

    func down(_ sample: TouchSample) {
        guard fingers[sample.id] == nil else { return }
        if fingers.isEmpty {
            started = sample.t
            count = 0
            moved = false
            consumed = false
            decision = .pending
            scroll = .zero
        }
        fingers[sample.id] = Finger(start: sample, last: sample)
        order.append(sample.id)
        count = max(count, fingers.count)
        clearTimer()
        if count == 1 {
            if mode == .trackpad, sample.t - lastTap < 300 {
                press()
                consumed = true
                lastTap = -.infinity
            } else if mode == .direct {
                cancelTimer = schedule(500) { [weak self] in
                    guard let self, self.fingers.count == 1, !self.moved else { return }
                    self.locate(CGPoint(x: sample.x, y: sample.y))
                    self.click(2)
                    self.consumed = true
                }
            }
        } else {
            release()
            if fingers.count == 2 {
                let position = pairPosition()
                pair = Pair(center: position.center, distance: position.distance, zoom: zoom)
            }
        }
    }

    func move(_ sample: TouchSample) {
        guard var finger = fingers[sample.id] else { return }
        let previous = finger.last
        finger.last = sample
        fingers[sample.id] = finger
        if distance(finger.start, sample) >= 10 {
            moved = true
            clearTimer()
        }
        if count == 1 {
            if consumed && !held { return }
            if mode == .direct {
                guard moved else { return }
                if !held {
                    locate(CGPoint(x: finger.start.x, y: finger.start.y))
                    press()
                }
                locate(CGPoint(x: sample.x, y: sample.y))
            } else {
                guard let area = geometry(), area.width > 0, area.height > 0 else { return }
                let speed = distance(previous, sample) / max(1, sample.t - previous.t)
                let gain = 1 + min(0.5, speed / 4)
                let sensitivity = CGFloat(pointerSensitivity)
                cursor = CGPoint(x: clamp(cursor.x + (sample.x - previous.x) / area.width * gain * sensitivity),
                                 y: clamp(cursor.y + (sample.y - previous.y) / area.height * gain * sensitivity))
                onCursor?(cursor)
                emit("move")
            }
            return
        }
        guard count == 2, fingers.count == 2, var pair else { return }
        let current = pairPosition()
        let ratio = current.distance / max(1, pair.distance)
        let dx = current.center.x - pair.center.x
        let dy = current.center.y - pair.center.y
        if decision == .pending {
            if abs(ratio - 1) > 0.15 && hypot(dx, dy) < 15 { decision = .pinch }
            else if hypot(dx, dy) >= 15 { decision = .scroll }
            else { return }
            moved = true
            consumed = true
        }
        if decision == .pinch {
            let scale = clamp(pair.zoom.scale * ratio, 1, 4)
            zoom = scale < 1.05 ? Zoom()
                : Zoom(scale: scale, offsetX: pair.zoom.offsetX + dx, offsetY: pair.zoom.offsetY + dy)
            onZoom?(zoom)
        } else {
            let speed = CGFloat(scrollSpeed)
            scroll.x -= dx * 5 * speed
            scroll.y -= dy * 5 * speed
            let deltaX = (scroll.x / 100).rounded(.towardZero) * 100
            let deltaY = (scroll.y / 100).rounded(.towardZero) * 100
            if deltaX != 0 || deltaY != 0 {
                send(PointerEvent(action: "wheel", x: cursor.x, y: cursor.y, deltaX: deltaX, deltaY: deltaY))
            }
            scroll.x -= deltaX
            scroll.y -= deltaY
            pair.center = current.center
            self.pair = pair
        }
    }

    func up(_ sample: TouchSample) {
        guard fingers[sample.id] != nil else { return }
        move(sample)
        clearTimer()
        release()
        fingers[sample.id] = nil
        order.removeAll { $0 == sample.id }
        guard fingers.isEmpty else { return }
        if !moved && !consumed && sample.t - started < 250 {
            if mode == .direct { locate(CGPoint(x: sample.x, y: sample.y)) }
            if count == 2 && sample.t - lastTwoTap < 300 {
                zoom = Zoom()
                onZoom?(zoom)
                lastTwoTap = -.infinity
            } else {
                click(count == 3 ? 1 : count == 2 ? 2 : 0)
                if count == 1 { lastTap = sample.t }
                if count == 2 { lastTwoTap = sample.t }
            }
        }
        pair = nil
    }

    func cancel() { reset() }

    func reset() {
        clearTimer()
        release()
        fingers.removeAll()
        order.removeAll()
        pair = nil
        lastTap = -.infinity
        lastTwoTap = -.infinity
    }

    private func clearTimer() {
        cancelTimer?()
        cancelTimer = nil
    }

    private func pairPosition() -> (center: CGPoint, distance: CGFloat) {
        let points = order.prefix(2).compactMap { fingers[$0]?.last }
        guard points.count == 2 else { return (.zero, 0) }
        let a = points[0], b = points[1]
        return (CGPoint(x: (a.x + b.x) / 2, y: (a.y + b.y) / 2), hypot(a.x - b.x, a.y - b.y))
    }

    private func locate(_ point: CGPoint) {
        guard let area = geometry(), area.width > 0, area.height > 0 else { return }
        cursor = CGPoint(x: clamp((point.x - area.minX) / area.width), y: clamp((point.y - area.minY) / area.height))
        onCursor?(cursor)
        emit("move")
    }

    private func emit(_ action: String, _ button: Int = 0) {
        send(PointerEvent(action: action, x: cursor.x, y: cursor.y, button: button))
    }

    private func press() {
        emit("down")
        held = true
    }

    private func release() {
        if held { emit("up") }
        held = false
    }

    private func click(_ button: Int) {
        emit("down", button)
        emit("up", button)
    }

    private func distance(_ a: TouchSample, _ b: TouchSample) -> CGFloat { hypot(a.x - b.x, a.y - b.y) }
    private func clamp(_ n: CGFloat, _ low: CGFloat = 0, _ high: CGFloat = 1) -> CGFloat { min(high, max(low, n)) }
}
