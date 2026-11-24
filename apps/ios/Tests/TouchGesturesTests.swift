import XCTest
@testable import Nanoctl

@MainActor
final class TouchGesturesTests: XCTestCase {
    private var events: [PointerEvent] = []
    private var zooms: [Zoom] = []
    private var timer: (() -> Void)?

    private func sample(_ id: Int, _ x: CGFloat = 100, _ y: CGFloat = 100, _ t: Double = 0) -> TouchSample {
        TouchSample(id: id, x: x, y: y, t: t)
    }

    private func setup(_ mode: TouchMode = .trackpad, area: CGRect? = CGRect(x: 0, y: 0, width: 1000, height: 500)) -> TouchGestures {
        events = []
        zooms = []
        timer = nil
        let gestures = TouchGestures(mode: mode, geometry: { area }, send: { [unowned self] in events.append($0) },
                                     schedule: { [unowned self] _, fire in
                                         timer = { MainActor.assumeIsolated { fire() } }
                                         return { [unowned self] in timer = nil }
                                     })
        gestures.onZoom = { [unowned self] in zooms.append($0) }
        return gestures
    }

    private var buttons: [PointerEvent] { events.filter { $0.action != "move" } }

    func testTrackpadTapClicksAtCursorAndDirectTapMovesToFinger() {
        for mode in TouchMode.allCases {
            let g = setup(mode)
            g.down(sample(1))
            g.up(sample(1, 100, 100, 100))
            let x = mode == .direct ? 0.1 : 0.5, y = mode == .direct ? 0.2 : 0.5
            XCTAssertEqual(buttons, [PointerEvent(action: "down", x: x, y: y), PointerEvent(action: "up", x: x, y: y)])
        }
    }

    func testTrackpadDragMovesRelativelyWithoutPressingAndDirectDragReleases() {
        for mode in TouchMode.allCases {
            let g = setup(mode)
            g.down(sample(1))
            g.move(sample(1, 200, 100, 100))
            g.up(sample(1, 200, 100, 300))
            XCTAssertGreaterThan(g.cursor.x, mode == .trackpad ? 0.5 : 0.1)
            XCTAssertEqual(buttons.map(\.action), mode == .direct ? ["down", "up"] : [])
        }
    }

    func testTrackpadMovementAcceleratesWithSpeed() {
        let slow = setup()
        slow.down(sample(1))
        slow.move(sample(1, 200, 100, 1000))
        let fast = setup()
        fast.down(sample(1))
        fast.move(sample(1, 200, 100, 10))
        XCTAssertEqual(slow.cursor.x, 0.6025, accuracy: 0.001)
        XCTAssertEqual(fast.cursor.x, 0.65, accuracy: 0.001)
    }

    func testDoubleTapAndHoldDragsAndCancelReleases() {
        let g = setup()
        g.down(sample(1))
        g.up(sample(1, 100, 100, 100))
        g.down(sample(1, 100, 100, 200))
        g.move(sample(1, 250, 100, 400))
        g.cancel()
        XCTAssertEqual(buttons.map(\.action), ["down", "up", "down", "up"])
        let count = events.count
        g.reset()
        XCTAssertEqual(events.count, count)
    }

    func testTwoAndThreeFingerTapsClickRightAndMiddle() {
        for fingers in [2, 3] {
            let g = setup()
            for id in 1...fingers { g.down(sample(id, CGFloat(id * 100))) }
            for id in 1...fingers { g.up(sample(id, CGFloat(id * 100), 100, 100)) }
            let button = fingers == 2 ? 2 : 1
            XCTAssertEqual(buttons.map(\.button), [button, button])
        }
    }

    func testParallelTwoFingerMotionEmitsWholeNaturalScrollSteps() {
        let g = setup()
        g.down(sample(1))
        g.down(sample(2, 200))
        g.move(sample(1, 100, 140, 100))
        g.move(sample(2, 200, 140, 100))
        g.up(sample(1, 100, 140, 200))
        g.up(sample(2, 200, 140, 200))
        XCTAssertTrue(events.contains { $0.action == "wheel" && $0.deltaY == -100 })
        XCTAssertTrue(events.allSatisfy { $0.action == "wheel" })
    }

    func testDirectLongPressRightClicksWithoutALeftClick() {
        let g = setup(.direct)
        g.down(sample(1))
        timer?()
        g.up(sample(1, 100, 100, 600))
        XCTAssertEqual(buttons.map(\.button), [2, 2])
    }

    func testPinchZoomsAndPansLocallyAndPinchingBackResets() {
        let g = setup()
        g.down(sample(1))
        g.down(sample(2, 200))
        g.move(sample(1, 80, 100, 100))
        XCTAssertEqual(g.zoom.scale, 1.2, accuracy: 0.001)
        g.move(sample(2, 230, 110, 110))
        XCTAssertEqual(g.zoom.offsetY, 5)
        g.move(sample(1, 130, 110, 120))
        XCTAssertEqual(g.zoom.scale, 1)
        g.up(sample(1, 130, 110, 130))
        g.up(sample(2, 230, 110, 130))
        XCTAssertFalse(zooms.isEmpty)
        XCTAssertTrue(events.isEmpty)
    }

    func testDoubleTwoFingerTapResetsZoom() {
        let g = setup()
        g.zoom = Zoom(scale: 2, offsetX: 20, offsetY: 30)
        for t in [0.0, 200] {
            g.down(sample(1, 100, 100, t))
            g.down(sample(2, 200, 100, t))
            g.up(sample(1, 100, 100, t + 50))
            g.up(sample(2, 200, 100, t + 50))
        }
        XCTAssertEqual(g.zoom, Zoom())
    }

    func testUnknownInputAndMissingGeometryAreHarmless() {
        let g = setup(.direct, area: nil)
        g.up(sample(1))
        g.move(sample(1))
        g.cancel()
        g.reset()
        XCTAssertTrue(events.isEmpty)
    }
}
