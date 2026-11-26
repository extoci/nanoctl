import UIKit

/// Browser-compatible key identity for the compact input wire. Printable keys
/// travel as their shifted character when one is available because host Unicode
/// injection does not necessarily combine with a separately pressed Shift key.
/// Key-up remains stable through `pressed` even if Shift is released first.
struct RemoteKey: Equatable {
    let code: String
    let value: String
    let named: UInt8?

    init(code: String, value: String, named: UInt8? = nil) {
        self.code = code
        self.value = value
        self.named = named
    }

    init?(_ key: UIKey) {
        if let named = Self.namedKeys[key.keyCode] {
            self.init(code: named.code, value: named.value, named: named.id)
            return
        }
        let character = [key.characters, key.charactersIgnoringModifiers].first { candidate in
            candidate.count == 1 && candidate.unicodeScalars.first.map { !CharacterSet.controlCharacters.contains($0) } == true
        }
        guard let character else { return nil }
        self.init(code: "", value: character)
    }

    private static let namedKeys: [UIKeyboardHIDUsage: (code: String, value: String, id: UInt8)] = [
        .keyboardDeleteOrBackspace: ("Backspace", "Backspace", 1),
        .keyboardDeleteForward: ("Delete", "Delete", 2),
        .keyboardReturnOrEnter: ("Enter", "Enter", 3),
        .keypadEnter: ("NumpadEnter", "Enter", 3),
        .keyboardEscape: ("Escape", "Escape", 4),
        .keyboardTab: ("Tab", "Tab", 5),
        .keyboardSpacebar: ("Space", " ", 6),
        .keyboardUpArrow: ("ArrowUp", "ArrowUp", 7),
        .keyboardDownArrow: ("ArrowDown", "ArrowDown", 8),
        .keyboardLeftArrow: ("ArrowLeft", "ArrowLeft", 9),
        .keyboardRightArrow: ("ArrowRight", "ArrowRight", 10),
        .keyboardHome: ("Home", "Home", 11),
        .keyboardEnd: ("End", "End", 12),
        .keyboardPageUp: ("PageUp", "PageUp", 13),
        .keyboardPageDown: ("PageDown", "PageDown", 14),
        .keyboardF1: ("F1", "F1", 15),
        .keyboardF2: ("F2", "F2", 16),
        .keyboardF3: ("F3", "F3", 17),
        .keyboardF4: ("F4", "F4", 18),
        .keyboardF5: ("F5", "F5", 19),
        .keyboardF6: ("F6", "F6", 20),
        .keyboardF7: ("F7", "F7", 21),
        .keyboardF8: ("F8", "F8", 22),
        .keyboardF9: ("F9", "F9", 23),
        .keyboardF10: ("F10", "F10", 24),
        .keyboardF11: ("F11", "F11", 25),
        .keyboardF12: ("F12", "F12", 26),
        .keyboardCapsLock: ("CapsLock", "CapsLock", 27),
        .keyboardPrintScreen: ("PrintScreen", "PrintScreen", 28),
        .keyboardInsert: ("Insert", "Insert", 29),
        .keyboardLeftShift: ("ShiftLeft", "Shift", 30),
        .keyboardRightShift: ("ShiftRight", "Shift", 30),
        .keyboardLeftControl: ("ControlLeft", "Control", 31),
        .keyboardRightControl: ("ControlRight", "Control", 31),
        .keyboardLeftAlt: ("AltLeft", "Alt", 32),
        .keyboardRightAlt: ("AltRight", "Alt", 32),
        .keyboardLeftGUI: ("MetaLeft", "Meta", 33),
        .keyboardRightGUI: ("MetaRight", "Meta", 33),
    ]

    static func special(_ name: String) -> RemoteKey? {
        namedKeys.first { $0.value.value == name || $0.value.code == name }.map {
            RemoteKey(code: $0.value.code, value: $0.value.value, named: $0.value.id)
        }
    }

    func binary(down: Bool, repeating: Bool) -> Data? {
        let flags = (down ? 0 : 1) | (repeating ? 2 : 0)
        if let named {
            return Data([5, UInt8(flags | 4), named])
        }
        let bytes = Array(value.utf8)
        guard !bytes.isEmpty, bytes.count <= 32 else { return nil }
        return Data([5, UInt8(flags)] + bytes)
    }
}

/// Owns keyboard press state and the sticky modifier keys shown above the
/// software keyboard. All sends stay on the reliable ordered control channel.
@MainActor
final class KeyboardForwarder {
    private let sendKey: (_ down: Bool, _ key: RemoteKey, _ repeating: Bool) -> Bool
    private let sendText: (_ text: String) -> Bool
    private var pressed: [UIKeyboardHIDUsage: RemoteKey] = [:]
    private var repeatSentAt: [UIKeyboardHIDUsage: TimeInterval] = [:]
    private var sticky: [RemoteKey] = []

    init(sendKey: @escaping (_ down: Bool, _ key: RemoteKey, _ repeating: Bool) -> Bool,
         sendText: @escaping (_ text: String) -> Bool) {
        self.sendKey = sendKey
        self.sendText = sendText
    }

    var stickyActive: Bool { !sticky.isEmpty }

    func isSticky(_ key: RemoteKey) -> Bool { sticky.contains(key) }

    @discardableResult
    func press(_ key: UIKey, down: Bool) -> Bool {
        guard let remote = RemoteKey(key) else { return false }
        if down {
            if let held = pressed[key.keyCode] {
                // The host synthesizes typematic input locally. Repeat
                // datagrams only prove that the viewer is still holding the
                // key, so throttle them like the web viewer. Reuse the initial
                // key identity even if modifiers changed while the key stayed down.
                let now = Date.timeIntervalSinceReferenceDate
                if now - (repeatSentAt[key.keyCode] ?? 0) < 1.0 { return true }
                repeatSentAt[key.keyCode] = now
                return sendKey(true, held, true)
            }
            pressed[key.keyCode] = remote
            return sendKey(true, remote, false)
        }
        repeatSentAt.removeValue(forKey: key.keyCode)
        let release = pressed.removeValue(forKey: key.keyCode) ?? remote
        return sendKey(false, release, false)
    }

    @discardableResult
    func tap(_ key: RemoteKey) -> Bool {
        let sent = sendKey(true, key, false) && sendKey(false, key, false)
        releaseSticky()
        return sent
    }

    @discardableResult
    func toggleSticky(_ key: RemoteKey) -> Bool {
        if let index = sticky.firstIndex(of: key) {
            sticky.remove(at: index)
            return sendKey(false, key, false)
        }
        sticky.append(key)
        return sendKey(true, key, false)
    }

    @discardableResult
    func insertText(_ text: String) -> Bool {
        guard !text.isEmpty else { return true }
        var sent = true
        var textToSend = text
        if stickyActive, let first = text.first {
            let onlyShift = sticky.allSatisfy { $0.value == "Shift" }
            if onlyShift {
                // Unicode text insertion preserves case even on hosts whose
                // keycode injection does not combine Shift with a printable key.
                textToSend = String(first).uppercased() + text.dropFirst()
            } else {
                let shifted = sticky.contains { $0.value == "Shift" }
                let key = RemoteKey(code: "", value: shifted ? String(first).uppercased() : String(first))
                sent = sendKey(true, key, false) && sendKey(false, key, false)
                textToSend = String(text.dropFirst())
            }
        }
        if !textToSend.isEmpty { sent = sendText(textToSend) && sent }
        releaseSticky()
        return sent
    }

    func releaseSticky() {
        let keys = sticky
        sticky.removeAll()
        for key in keys { _ = sendKey(false, key, false) }
    }

    func releaseAll() {
        releaseSticky()
        for key in pressed.values { _ = sendKey(false, key, false) }
        pressed.removeAll()
        repeatSentAt.removeAll()
    }
}

/// Invisible UIKeyInput sink. It exists only to summon the software keyboard;
/// committed text is forwarded remotely instead of accumulating locally.
@MainActor
final class KeyboardInputView: UIView, UIKeyInput {
    let forwarder: KeyboardForwarder
    private let accessory: KeyboardAccessoryView
    private let emptyInputView = UIView(frame: .zero)

    var keyboardType: UIKeyboardType = .default
    var returnKeyType: UIReturnKeyType = .default
    var autocorrectionType: UITextAutocorrectionType = .no
    var autocapitalizationType: UITextAutocapitalizationType = .none
    var spellCheckingType: UITextSpellCheckingType = .no
    var smartQuotesType: UITextSmartQuotesType = .no
    var smartDashesType: UITextSmartDashesType = .no
    var smartInsertDeleteType: UITextSmartInsertDeleteType = .no

    var hasText: Bool { true }
    var wantsSoftwareKeyboard = false
    override var canBecomeFirstResponder: Bool { true }
    override var inputView: UIView? { wantsSoftwareKeyboard ? nil : emptyInputView }
    override var inputAccessoryView: UIView? { wantsSoftwareKeyboard ? accessory : nil }

    init(forwarder: KeyboardForwarder) {
        self.forwarder = forwarder
        accessory = KeyboardAccessoryView(forwarder: forwarder)
        super.init(frame: CGRect(x: 0, y: 0, width: 1, height: 1))
        alpha = 0.01
        isUserInteractionEnabled = false
    }

    required init?(coder: NSCoder) { nil }

    func insertText(_ text: String) {
        if text == "\n" || text == "\r" {
            if let enter = RemoteKey.special("Enter") { _ = forwarder.tap(enter) }
        } else {
            _ = forwarder.insertText(text)
        }
        accessory.refresh()
    }

    func deleteBackward() {
        if let backspace = RemoteKey.special("Backspace") { _ = forwarder.tap(backspace) }
        accessory.refresh()
    }

    override func pressesBegan(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        forward(presses, down: event, began: true)
    }

    override func pressesEnded(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        forward(presses, down: event, began: false)
    }

    override func pressesCancelled(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        forward(presses, down: event, began: false)
    }

    private func forward(_ presses: Set<UIPress>, down event: UIPressesEvent?, began: Bool) {
        var remaining = Set<UIPress>()
        for press in presses {
            guard let key = press.key, forwarder.press(key, down: began) else {
                remaining.insert(press)
                continue
            }
        }
        guard !remaining.isEmpty else { return }
        if began { super.pressesBegan(remaining, with: event) }
        else { super.pressesEnded(remaining, with: event) }
    }
}

private final class KeyboardAccessoryView: UIView {
    private struct Item {
        let title: String
        let key: RemoteKey
        let modifier: Bool
    }

    private let forwarder: KeyboardForwarder
    private var buttons: [ObjectIdentifier: RemoteKey] = [:]
    private var modifierButtons: [ObjectIdentifier: UIButton] = [:]
    private let scroll = UIScrollView()
    private let stack = UIStackView()

    init(forwarder: KeyboardForwarder) {
        self.forwarder = forwarder
        super.init(frame: CGRect(x: 0, y: 0, width: UIScreen.main.bounds.width, height: 46))
        autoresizingMask = [.flexibleWidth, .flexibleHeight]
        backgroundColor = UIColor.black.withAlphaComponent(0.82)
        scroll.showsHorizontalScrollIndicator = false
        stack.axis = .horizontal
        stack.spacing = 7
        stack.alignment = .center
        stack.layoutMargins = UIEdgeInsets(top: 5, left: 8, bottom: 5, right: 8)
        stack.isLayoutMarginsRelativeArrangement = true
        scroll.addSubview(stack)
        addSubview(scroll)
        scroll.translatesAutoresizingMaskIntoConstraints = false
        stack.translatesAutoresizingMaskIntoConstraints = false
        NSLayoutConstraint.activate([
            scroll.leadingAnchor.constraint(equalTo: leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: trailingAnchor),
            scroll.topAnchor.constraint(equalTo: topAnchor),
            scroll.bottomAnchor.constraint(equalTo: bottomAnchor),
            stack.leadingAnchor.constraint(equalTo: scroll.contentLayoutGuide.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: scroll.contentLayoutGuide.trailingAnchor),
            stack.topAnchor.constraint(equalTo: scroll.contentLayoutGuide.topAnchor),
            stack.bottomAnchor.constraint(equalTo: scroll.contentLayoutGuide.bottomAnchor),
            stack.heightAnchor.constraint(equalTo: scroll.frameLayoutGuide.heightAnchor),
        ])
        for item in Self.items { addButton(item) }
    }

    required init?(coder: NSCoder) { nil }

    override var intrinsicContentSize: CGSize { CGSize(width: UIView.noIntrinsicMetric, height: 46) }

    private static var items: [Item] {
        func key(_ name: String) -> RemoteKey? { RemoteKey.special(name) }
        return [
            ("Esc", key("Escape"), false),
            ("Tab", key("Tab"), false),
            ("Shift", key("Shift"), true),
            ("Ctrl", key("Control"), true),
            ("Alt", key("Alt"), true),
            ("Win", key("Meta"), true),
            ("←", key("ArrowLeft"), false),
            ("↑", key("ArrowUp"), false),
            ("↓", key("ArrowDown"), false),
            ("→", key("ArrowRight"), false),
            ("Home", key("Home"), false),
            ("End", key("End"), false),
            ("PgUp", key("PageUp"), false),
            ("PgDn", key("PageDown"), false),
            ("Del", key("Delete"), false),
        ].compactMap { item in
            item.1.map { Item(title: item.0, key: $0, modifier: item.2) }
        }
    }

    private func addButton(_ item: Item) {
        var configuration = UIButton.Configuration.plain()
        configuration.title = item.title
        configuration.baseForegroundColor = .white
        configuration.background.backgroundColor = UIColor.white.withAlphaComponent(0.13)
        configuration.background.cornerRadius = 8
        configuration.contentInsets = NSDirectionalEdgeInsets(top: 5, leading: 10, bottom: 5, trailing: 10)
        let button = UIButton(configuration: configuration)
        button.addTarget(self, action: #selector(tapped(_:)), for: .touchUpInside)
        button.tag = item.modifier ? 1 : 0
        buttons[ObjectIdentifier(button)] = item.key
        if item.modifier { modifierButtons[ObjectIdentifier(button)] = button }
        stack.addArrangedSubview(button)
    }

    @objc private func tapped(_ button: UIButton) {
        guard let key = buttons[ObjectIdentifier(button)] else { return }
        if button.tag == 1 { _ = forwarder.toggleSticky(key) }
        else { _ = forwarder.tap(key) }
        refresh()
    }

    func refresh() {
        for (id, button) in modifierButtons {
            guard let key = buttons[id] else { continue }
            button.configuration?.background.backgroundColor = forwarder.isSticky(key)
                ? UIColor.systemBlue.withAlphaComponent(0.8)
                : UIColor.white.withAlphaComponent(0.13)
        }
    }
}
