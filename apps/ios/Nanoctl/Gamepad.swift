import GameController
import UIKit

@MainActor
final class Gamepad {
    private let session: Session
    private var active = false
    /// Stable host slots. A controller keeps its slot across other controllers
    /// connecting or disconnecting; slot identity matters because Windows sees
    /// these as independent virtual gamepads.
    private var controllers: [GCController?] = Array(repeating: nil, count: 4)
    private var keepalive: Timer?
    private var lastSent: GamepadMessage?
    private var changedAt: TimeInterval?
    private var fullAt: TimeInterval?
    private var heartbeatAt: TimeInterval?
    var onChange: (() -> Void)?

    init(session: Session) {
        self.session = session
        NotificationCenter.default.addObserver(self, selector: #selector(controllerNotification),
                                               name: .GCControllerDidConnect, object: nil)
        NotificationCenter.default.addObserver(self, selector: #selector(controllerNotification),
                                               name: .GCControllerDidDisconnect, object: nil)
        controllersChanged()
    }

    var status: String {
        let connected = controllers.compactMap { $0 }
        guard !connected.isEmpty else { return "No gamepad paired" }
        let names = connected.map { controller in
            let profile = controller.extendedGamepad
            if profile is GCDualSenseGamepad { return "DualSense → DualShock 4" }
            if profile is GCDualShockGamepad { return "DualShock → DualShock 4" }
            return "\(controller.vendorName ?? "Gamepad") → Xbox 360"
        }
        let name = names.count == 1 ? names[0] : "\(names.count) gamepads · \(names.joined(separator: " + "))"
        return session.gamepadAllowed ? name : "\(name) · host gamepad unavailable"
    }

    func setActive(_ active: Bool) {
        self.active = active
        keepalive?.invalidate()
        keepalive = nil
        if active {
            // The host neutralizes pads after 750 ms without a report. The
            // 50 ms timer supplies repeat, heartbeat, and refresh deadlines.
            let timer = Timer(timeInterval: 0.05, repeats: true) { [weak self] _ in
                MainActor.assumeIsolated { self?.send(force: true) }
            }
            RunLoop.main.add(timer, forMode: .common)
            keepalive = timer
            send(force: true)
        } else {
            release()
        }
    }

    @objc nonisolated private func controllerNotification() {
        DispatchQueue.main.async { [weak self] in self?.controllersChanged() }
    }

    private func controllersChanged() {
        let available = GCController.controllers().filter { $0.extendedGamepad != nil }
        var next: [GCController?] = Array(repeating: nil, count: controllers.count)

        // Keep existing controllers in their old slots so a second player
        // joining does not move the first player's virtual controller.
        for (slot, controller) in controllers.enumerated() {
            if let controller, available.contains(controller) { next[slot] = controller }
        }
        for controller in available where !next.contains(where: { $0 === controller }) {
            guard let slot = next.firstIndex(where: { $0 == nil }) else { break }
            next[slot] = controller
        }

        let unchanged = controllers.map { $0.map(ObjectIdentifier.init) } == next.map { $0.map(ObjectIdentifier.init) }
        if unchanged { return }
        let retained = Set(next.compactMap { $0 }.map(ObjectIdentifier.init))
        for controller in controllers.compactMap({ $0 }) where !retained.contains(ObjectIdentifier(controller)) {
            controller.extendedGamepad?.valueChangedHandler = nil
        }
        controllers = next
        for controller in controllers.compactMap({ $0 }) {
            controller.handlerQueue = .main
            controller.extendedGamepad?.valueChangedHandler = { [weak self] _, _ in
                MainActor.assumeIsolated { self?.send() }
            }
        }

        // The next report must be full: it is both the sparse baseline and the
        // host's detach signal for any slot that disappeared.
        resetState()
        send(force: true)
        onChange?()
    }

    private func resetState() {
        lastSent = nil
        changedAt = nil
        fullAt = nil
        heartbeatAt = nil
    }

    private func release() {
        if lastSent != nil { session.sendGamepad(GamepadMessage(pads: [])) }
        resetState()
    }

    private func report(_ pad: GCExtendedGamepad, slot: Int) -> [Int] {
        let digital: [(GCControllerButtonInput?, Int)] = [
            (pad.buttonA, 0x1000), (pad.buttonB, 0x2000), (pad.buttonX, 0x4000), (pad.buttonY, 0x8000),
            (pad.leftShoulder, 0x0100), (pad.rightShoulder, 0x0200),
            (pad.buttonOptions, 0x0020), (pad.buttonMenu, 0x0010), (pad.buttonHome, 0x0400),
            (pad.leftThumbstickButton, 0x0040), (pad.rightThumbstickButton, 0x0080),
            (pad.dpad.up, 0x0001), (pad.dpad.down, 0x0002), (pad.dpad.left, 0x0004), (pad.dpad.right, 0x0008),
        ]
        let mask = digital.reduce(0) { $0 | ($1.0?.isPressed == true ? $1.1 : 0) }
        return [slot, mask, GamepadMessage.trigger(pad.leftTrigger.value), GamepadMessage.trigger(pad.rightTrigger.value),
                GamepadMessage.axis(pad.leftThumbstick.xAxis.value), GamepadMessage.axis(pad.leftThumbstick.yAxis.value),
                GamepadMessage.axis(pad.rightThumbstick.xAxis.value), GamepadMessage.axis(pad.rightThumbstick.yAxis.value)]
    }

    private func currentMessage() -> GamepadMessage {
        var pads: [[Int]] = []
        var sony: [GamepadMessage.DualShock4] = []
        for (slot, controller) in controllers.enumerated() {
            guard let pad = controller?.extendedGamepad else { continue }
            pads.append(report(pad, slot: slot))
            if pad is GCDualSenseGamepad || pad is GCDualShockGamepad {
                sony.append(.init(slot: slot, leftTriggerPressed: pad.leftTrigger.isPressed,
                                  rightTriggerPressed: pad.rightTrigger.isPressed, touchpadPressed: false))
            }
        }
        var message = GamepadMessage(pads: pads)
        if !sony.isEmpty { message.dualShock4 = sony }
        return message
    }

    private func send(force: Bool = false) {
        guard active else { return }
        let message = currentMessage()
        if message.pads.isEmpty {
            // A full empty report is the host-side detach for every slot. Send
            // it once after the last controller disappears, then stay quiet.
            guard lastSent == nil else { return }
            if session.sendGamepad(message) {
                lastSent = message
                fullAt = Date.timeIntervalSinceReferenceDate
                heartbeatAt = fullAt
            }
            return
        }

        let now = Date.timeIntervalSinceReferenceDate
        let changed = message != lastSent
        if changed { changedAt = now }
        // Bounded SCTP retransmission now repairs loss; keep only a brief final-state anchor.
        let repeating = changedAt.map { now - $0 < 0.03 } ?? false
        let needsFull = fullAt.map { now - $0 >= 2.0 } ?? true
        let heartbeatDue = heartbeatAt.map { now - $0 >= 0.25 } ?? true
        var sent = false
        if changed || repeating || needsFull {
            let sparse = !needsFull
            sent = session.sendGamepad(message, sparse: sparse)
            if sent {
                if !sparse { fullAt = now }
                heartbeatAt = now
            } else {
                // Rebase after a transport outage: a new host-side gamepad stream
                // cannot use sparse state until it receives a full snapshot.
                fullAt = nil
            }
        } else if force && heartbeatDue {
            sent = session.sendGamepadHeartbeat()
            if sent { heartbeatAt = now }
        }
        if sent { lastSent = message }
    }
}
