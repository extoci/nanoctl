import GameController
import UIKit

@MainActor
final class Gamepad {
    private let session: Session
    private var active = false
    private var controller: GCController?
    private var keepalive: Timer?
    private var lastSent: GamepadMessage?
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
        guard let controller else { return "No gamepad paired" }
        let profile = controller.extendedGamepad
        let name: String
        if profile is GCDualSenseGamepad { name = "DualSense → DualShock 4" }
        else if profile is GCDualShockGamepad { name = "DualShock → DualShock 4" }
        else { name = "\(controller.vendorName ?? "Gamepad") → Xbox 360" }
        return session.gamepadAllowed ? name : "\(name) · host gamepad unavailable"
    }

    func setActive(_ active: Bool) {
        self.active = active
        keepalive?.invalidate()
        keepalive = nil
        if active {
            let timer = Timer(timeInterval: 0.5, repeats: true) { [weak self] _ in
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
        if let controller, available.contains(controller) { return }
        release()
        controller?.extendedGamepad?.valueChangedHandler = nil
        controller = available.first
        controller?.handlerQueue = .main
        controller?.extendedGamepad?.valueChangedHandler = { [weak self] _, _ in
            MainActor.assumeIsolated { self?.send() }
        }
        send(force: true)
        onChange?()
    }

    private func release() {
        if lastSent != nil { session.sendGamepad(GamepadMessage(pads: [])) }
        lastSent = nil
    }

    private func send(force: Bool = false) {
        guard active, let pad = controller?.extendedGamepad else { return }
        let digital: [(GCControllerButtonInput?, Int)] = [
            (pad.buttonA, 0x1000), (pad.buttonB, 0x2000), (pad.buttonX, 0x4000), (pad.buttonY, 0x8000),
            (pad.leftShoulder, 0x0100), (pad.rightShoulder, 0x0200),
            (pad.buttonOptions, 0x0020), (pad.buttonMenu, 0x0010), (pad.buttonHome, 0x0400),
            (pad.leftThumbstickButton, 0x0040), (pad.rightThumbstickButton, 0x0080),
            (pad.dpad.up, 0x0001), (pad.dpad.down, 0x0002), (pad.dpad.left, 0x0004), (pad.dpad.right, 0x0008),
        ]
        let mask = digital.reduce(0) { $0 | ($1.0?.isPressed == true ? $1.1 : 0) }
        let report = [0, mask, GamepadMessage.trigger(pad.leftTrigger.value), GamepadMessage.trigger(pad.rightTrigger.value),
                      GamepadMessage.axis(pad.leftThumbstick.xAxis.value), GamepadMessage.axis(pad.leftThumbstick.yAxis.value),
                      GamepadMessage.axis(pad.rightThumbstick.xAxis.value), GamepadMessage.axis(pad.rightThumbstick.yAxis.value)]
        var message = GamepadMessage(pads: [report])
        if pad is GCDualSenseGamepad || pad is GCDualShockGamepad {
            message.dualShock4 = [.init(slot: 0, leftTriggerPressed: pad.leftTrigger.isPressed,
                                       rightTriggerPressed: pad.rightTrigger.isPressed, touchpadPressed: false)]
        }
        if (force || message != lastSent), session.sendGamepad(message) { lastSent = message }
    }
}
