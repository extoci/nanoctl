import AVKit
import SwiftUI

struct RootView: View {
    let desktop: Desktop

    var body: some View {
        let model = desktop.model
        ZStack {
            if model.active {
                SessionView(desktop: desktop)
                    .transition(.opacity)
            } else {
                ConnectView(desktop: desktop)
                    .transition(.opacity)
            }
        }
        .overlay(alignment: .bottom) {
            if let toast = model.toast {
                Text(toast)
                    .font(.callout)
                    .multilineTextAlignment(.center)
                    .padding(.horizontal, 18)
                    .padding(.vertical, 12)
                    .glass(in: Capsule(), interactive: false)
                    .padding(24)
                    .transition(.move(edge: .bottom).combined(with: .opacity))
                    .allowsHitTesting(false)
            }
        }
        .animation(.smooth, value: model.active)
        .animation(.smooth, value: model.toast)
        .preferredColorScheme(.dark)
    }
}

// MARK: - Connect

struct ConnectView: View {
    static func label(_ host: String) -> String {
        guard let url = URL(string: host), let name = url.host() else { return host }
        return url.port.map { "\(name):\($0)" } ?? name
    }

    let desktop: Desktop
    @State private var address = UserDefaults.standard.string(forKey: "host") ?? ""
    @State private var showLogs = false
    @FocusState private var focused: Bool

    var body: some View {
        let model = desktop.model
        ZStack {
            LinearGradient(colors: [Color(red: 0.05, green: 0.07, blue: 0.12), .black],
                           startPoint: .top, endPoint: .bottom)
                .ignoresSafeArea()
            ScrollView {
                VStack(spacing: 28) {
                    VStack(spacing: 10) {
                        Image(systemName: "display")
                            .font(.system(size: 44, weight: .light))
                            .foregroundStyle(.secondary)
                        Text("nanoctl")
                            .font(.largeTitle.bold())
                        Text("Connect to a host on your tailnet.")
                            .foregroundStyle(.secondary)
                    }
                    .padding(.top, 72)

                    GlassGroup(spacing: 12) {
                        VStack(spacing: 12) {
                            HStack(spacing: 10) {
                                TextField("host.tailnet.ts.net", text: $address)
                                    .textContentType(.URL)
                                    .keyboardType(.URL)
                                    .textInputAutocapitalization(.never)
                                    .autocorrectionDisabled()
                                    .submitLabel(.go)
                                    .focused($focused)
                                    .onSubmit(connect)
                                    .padding(.horizontal, 20)
                                    .frame(height: 52)
                                    .glass(in: Capsule())
                                Button {
                                    focused = false
                                    desktop.refreshHosts(extraSeed: address)
                                } label: {
                                    Image(systemName: model.discoveringHosts ? "antenna.radiowaves.left.and.right" : "magnifyingglass")
                                        .font(.system(size: 17, weight: .semibold))
                                        .frame(width: 52, height: 52)
                                }
                                .buttonStyle(.plain)
                                .glass(in: Circle())
                                .accessibilityLabel("Find hosts")
                            }
                            Button(action: connect) {
                                Text("Connect")
                                    .font(.headline)
                                    .frame(maxWidth: .infinity)
                                    .frame(height: 36)
                            }
                            .glassButton(prominent: true)
                            .controlSize(.large)
                            .disabled(address.trimmingCharacters(in: .whitespaces).isEmpty)
                        }
                    }
                    .frame(maxWidth: 420)

                    if model.status != "Disconnected" {
                        Text(model.status)
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                            .multilineTextAlignment(.center)
                    }

                    if model.discoveringHosts || !model.discoveredHosts.isEmpty || model.discoveryNotice != nil {
                        VStack(alignment: .leading, spacing: 10) {
                            HStack {
                                Text("On your tailnet")
                                    .font(.footnote.weight(.semibold))
                                    .foregroundStyle(.secondary)
                                Spacer()
                                if model.discoveringHosts {
                                    ProgressView()
                                        .controlSize(.small)
                                } else if let notice = model.discoveryNotice {
                                    Text(notice)
                                        .font(.caption2)
                                        .foregroundStyle(.tertiary)
                                }
                            }
                            .padding(.horizontal, 8)
                            if model.discoveredHosts.isEmpty, model.discoveringHosts {
                                HStack(spacing: 12) {
                                    ProgressView()
                                    Text("Looking for nanoctl hosts…")
                                        .font(.subheadline)
                                        .foregroundStyle(.secondary)
                                }
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .padding(.horizontal, 18)
                                .frame(height: 54)
                                .glass(in: Capsule(), interactive: false)
                            } else {
                                GlassGroup(spacing: 8) {
                                    VStack(spacing: 8) {
                                        ForEach(model.discoveredHosts) { host in
                                            Button {
                                                address = host.url
                                                connect()
                                            } label: {
                                                HStack(spacing: 13) {
                                                    Image(systemName: hostIcon(host.os))
                                                        .font(.title3)
                                                        .frame(width: 28)
                                                    VStack(alignment: .leading, spacing: 3) {
                                                        Text(host.displayName)
                                                            .font(.subheadline.weight(.semibold))
                                                            .lineLimit(1)
                                                        Text(host.endpointLabel)
                                                            .font(.caption)
                                                            .foregroundStyle(.secondary)
                                                            .lineLimit(1)
                                                    }
                                                    Spacer(minLength: 8)
                                                    Text(host.online ? "Ready" : "Offline")
                                                        .font(.caption2.weight(.semibold))
                                                        .foregroundStyle(host.online ? .green : .secondary)
                                                    Image(systemName: "play.fill")
                                                        .font(.caption)
                                                        .foregroundStyle(.secondary)
                                                }
                                                .padding(.horizontal, 18)
                                                .frame(height: 58)
                                                .contentShape(RoundedRectangle(cornerRadius: 24, style: .continuous))
                                            }
                                            .buttonStyle(.plain)
                                            .glass(in: RoundedRectangle(cornerRadius: 24, style: .continuous))
                                            .disabled(!host.online)
                                            .opacity(host.online ? 1 : 0.55)
                                        }
                                    }
                                }
                            }
                        }
                        .frame(maxWidth: 420)
                    }

                    if !model.recents.isEmpty {
                        VStack(alignment: .leading, spacing: 10) {
                            Text("Recent")
                                .font(.footnote.weight(.semibold))
                                .foregroundStyle(.secondary)
                                .padding(.leading, 8)
                            GlassGroup(spacing: 8) {
                                VStack(spacing: 8) {
                                    ForEach(model.recents, id: \.self) { host in
                                        Button { address = host; connect() } label: {
                                            HStack {
                                                Image(systemName: "desktopcomputer")
                                                Text(Self.label(host))
                                                    .lineLimit(1)
                                                Spacer()
                                                if URL(string: host)?.scheme == "http" {
                                                    Text("HTTP")
                                                        .font(.caption2.weight(.semibold))
                                                        .foregroundStyle(.secondary)
                                                }
                                                Image(systemName: "arrow.right")
                                                    .foregroundStyle(.secondary)
                                            }
                                            .padding(.horizontal, 18)
                                            .frame(height: 48)
                                            .contentShape(Capsule())
                                        }
                                        .buttonStyle(.plain)
                                        .glass(in: Capsule())
                                        .contextMenu {
                                            Button("Forget", systemImage: "trash", role: .destructive) {
                                                desktop.forget(host)
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        .frame(maxWidth: 420)
                    }
                    VStack(alignment: .leading, spacing: 10) {
                        Text("Controllers")
                            .font(.footnote.weight(.semibold))
                            .foregroundStyle(.secondary)
                            .padding(.leading, 8)
                        GamepadStatusList(statuses: model.gamepads)
                    }
                    .frame(maxWidth: 420)

                    HStack(spacing: 14) {
                        Text(BuildInfo.versionLine)
                            .font(.caption2.monospaced())
                            .foregroundStyle(.tertiary)
                        Button {
                            showLogs = true
                        } label: {
                            Label("Logs", systemImage: "doc.text.magnifyingglass")
                                .font(.caption.weight(.medium))
                        }
                        .buttonStyle(.plain)
                        .foregroundStyle(.secondary)
                    }
                    .padding(.bottom, 8)
                }
                .padding(24)
                .frame(maxWidth: .infinity)
            }
            .scrollDismissesKeyboard(.interactively)
        }
        .sheet(isPresented: $showLogs) {
            DebugLogView()
        }
        .onAppear { desktop.refreshHosts() }
    }

    private func connect() {
        focused = false
        desktop.connect(address)
    }

    private func hostIcon(_ os: String?) -> String {
        switch os?.lowercased() {
        case .some(let value) where value.contains("windows"): return "pc"
        case .some(let value) where value.contains("mac"): return "desktopcomputer"
        case .some(let value) where value.contains("linux"): return "server.rack"
        default: return "display"
        }
    }
}

// MARK: - Session

struct ConnectProgressView: View {
    let progress: ConnectProgress
    private let stages: [ConnectStage] = [.signaling, .networking, .media]

    var body: some View {
        VStack(spacing: 10) {
            HStack(spacing: 7) {
                ForEach(stages, id: \.self) { stage in
                    step(stage)
                    if stage != .media {
                        Rectangle()
                            .fill(stage.rawValue < progress.stage.rawValue ? Color.green : Color.white.opacity(0.2))
                            .frame(width: 13, height: 1)
                    }
                }
            }
            Text(progress.detail)
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
            if let hint = progress.failureHint {
                Text(hint)
                    .font(.caption)
                    .foregroundStyle(.orange)
                    .multilineTextAlignment(.center)
            }
        }
    }

    private func step(_ stage: ConnectStage) -> some View {
        let complete = progress.stage == .live || stage.rawValue < progress.stage.rawValue
        let current = stage.rawValue == progress.stage.rawValue
        let failed = current && progress.failure != nil
        return VStack(spacing: 4) {
            Image(systemName: complete ? "checkmark.circle.fill" : failed ? "exclamationmark.triangle.fill" : current ? "circle.fill" : "circle")
                .font(.system(size: 15))
                .foregroundStyle(complete ? Color.green : failed ? Color.orange : current ? Color.blue : Color.white.opacity(0.3))
            Text(stage.label)
                .font(.caption2)
                .foregroundStyle(current || complete ? Color.white : Color.white.opacity(0.45))
        }
        .frame(width: 72)
    }
}

/// The desktop fills the whole screen; controls live in one glass button
/// that expands on tap and gets out of the way otherwise.
struct SessionView: View {
    let desktop: Desktop
    @State private var expanded = false
    @State private var showControllers = false
    @State private var collapseTask: Task<Void, Never>?
    @Namespace private var glass

    var body: some View {
        let model = desktop.model
        ZStack {
            StageView(stage: desktop.stage)
                .ignoresSafeArea()

            if model.external {
                VStack(spacing: 8) {
                    Image(systemName: "tv")
                        .font(.system(size: 40, weight: .light))
                    Text("Showing on your TV")
                        .font(.headline)
                    Text("Use this screen as a trackpad.")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                }
                .foregroundStyle(.white.opacity(0.6))
                .allowsHitTesting(false)
            } else if model.connectProgress.stage != .live {
                VStack(spacing: 16) {
                    ProgressView()
                        .controlSize(.large)
                    Text(model.hostName ?? "nanoctl")
                        .font(.headline)
                    ConnectProgressView(progress: model.connectProgress)
                    Button("Cancel") { desktop.session.stop() }
                        .glassButton()
                }
                .padding(28)
                .frame(maxWidth: 320)
                .glass(in: RoundedRectangle(cornerRadius: 32, style: .continuous), interactive: false)
            }
        }
        .overlay(alignment: .bottomLeading) {
            if model.showStats, !model.stats.isEmpty {
                Text(model.stats)
                    .font(.caption.monospaced())
                    .multilineTextAlignment(.leading)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 8)
                    .glass(in: RoundedRectangle(cornerRadius: 14, style: .continuous), interactive: false)
                    .padding(12)
                    .allowsHitTesting(false)
            }
        }
        .overlay(alignment: .topTrailing) {
            chrome(model)
                .padding(12)
        }
        .statusBarHidden()
        .persistentSystemOverlays(.hidden)
        .defersSystemGestures(on: .all)
        .animation(.smooth(duration: 0.3), value: expanded)
        .sheet(isPresented: $showControllers) {
            GamepadSheet(statuses: model.gamepads)
        }
    }

    @ViewBuilder
    private func chrome(_ model: ViewerModel) -> some View {
        GlassGroup(spacing: 8) {
            HStack(spacing: 8) {
                if expanded {
                    AirPlayButton()
                        .frame(width: 44, height: 44)
                        .glass(in: Circle())
                        .glassID("airplay", in: glass)
                        .accessibilityLabel("AirPlay")

                    ChromeButton(model.touchMode == .trackpad ? "Trackpad" : "Direct touch",
                                 systemImage: model.touchMode == .trackpad ? "rectangle.and.hand.point.up.left"
                                                                           : "hand.point.up.left") {
                        if model.external {
                            desktop.show("Trackpad input is required on an external display")
                        } else {
                            desktop.setTouchMode(model.touchMode == .trackpad ? .direct : .trackpad)
                            desktop.show(model.touchMode == .trackpad
                                         ? "Trackpad: drag to move, tap to click"
                                         : "Direct touch: tap where you want to click")
                        }
                        poke()
                    }
                    .glassID("touch", in: glass)

                    ChromeButton(model.keyboardVisible ? "Hide keyboard" : "Keyboard",
                                 systemImage: model.keyboardVisible ? "keyboard.chevron.compact.down" : "keyboard") {
                        desktop.setKeyboardVisible(!model.keyboardVisible)
                        poke()
                    }
                    .opacity(model.keyboardVisible ? 1 : 0.92)
                    .glassID("keyboard", in: glass)

                    ChromeButton("Controllers", systemImage: "gamecontroller.fill") {
                        showControllers = true
                        poke()
                    }
                    .overlay(alignment: .topTrailing) {
                        if !model.gamepads.isEmpty {
                            Text("\(model.gamepads.count)")
                                .font(.system(size: 10, weight: .bold).monospacedDigit())
                                .foregroundStyle(.white)
                                .frame(width: 17, height: 17)
                                .background(.green.gradient, in: Circle())
                                .offset(x: 2, y: -2)
                                .allowsHitTesting(false)
                        }
                    }
                    .accessibilityHint("Shows controller type, player slot, host device, and battery")
                    .glassID("controllers", in: glass)

                    Menu {
                        menu(model)
                            .onAppear { collapseTask?.cancel() }
                            .onDisappear {
                                desktop.restoreInputFocus()
                                poke()
                            }
                    } label: {
                        Image(systemName: "ellipsis")
                            .font(.system(size: 17, weight: .semibold))
                            .frame(width: 44, height: 44)
                            .contentShape(Circle())
                    }
                    .glass(in: Circle())
                    .glassID("more", in: glass)
                    .accessibilityLabel("More")
                }

                ChromeButton(expanded ? "Hide controls" : "Show controls",
                             systemImage: expanded ? "xmark" : "chevron.down") {
                    expanded.toggle()
                    poke()
                }
                .opacity(expanded ? 1 : 0.7)
                .glassID("toggle", in: glass)
            }
        }
        .foregroundStyle(.white)
    }

    @ViewBuilder
    private func menu(_ model: ViewerModel) -> some View {
        Picker("Touch", selection: Binding(get: { model.touchMode }, set: { desktop.setTouchMode($0) })) {
            Label("Trackpad", systemImage: "rectangle.and.hand.point.up.left").tag(TouchMode.trackpad)
            Label("Direct touch", systemImage: "hand.point.up.left").tag(TouchMode.direct)
        }
        .disabled(model.external)

        Menu {
            Picker("Trackpad speed", selection: Binding(get: { model.pointerSensitivity }, set: {
                desktop.setPointerSensitivity($0)
            })) {
                ForEach([0.5, 0.75, 1.0, 1.25, 1.5], id: \.self) { value in
                    Text("\(Int(value * 100))%").tag(value)
                }
            }
            .pickerStyle(.menu)
            Picker("Scroll speed", selection: Binding(get: { model.scrollSpeed }, set: {
                desktop.setScrollSpeed($0)
            })) {
                ForEach([0.5, 0.75, 1.0, 1.25, 1.5], id: \.self) { value in
                    Text("\(Int(value * 100))%").tag(value)
                }
            }
            .pickerStyle(.menu)
        } label: {
            Label("Input feel", systemImage: "hand.draw")
        }

        Section {
            if model.audioAvailable {
                Toggle(isOn: Binding(get: { model.audioEnabled }, set: {
                    desktop.session.audioEnabled = $0
                    desktop.update()
                })) {
                    Label("Sound", systemImage: "speaker.wave.2")
                }
            }
            if !model.audioOutputs.isEmpty {
                Menu {
                    Picker("Host output", selection: Binding(get: { model.selectedAudioOutput ?? "" }, set: { output in
                        if !desktop.session.selectAudioOutput(output.isEmpty ? nil : output) {
                            desktop.show("Could not change the host audio output")
                        }
                    })) {
                        Text("System default").tag("")
                        ForEach(model.audioOutputs) { output in
                            Text(output.isDefault == true ? "\(output.name) (default)" : output.name)
                                .tag(output.id)
                        }
                    }
                    .pickerStyle(.menu)
                } label: {
                    Label("Host output", systemImage: "hifispeaker")
                }
            }
            if model.clipboard {
                Button("Paste to host", systemImage: "doc.on.clipboard") {
                    if !desktop.session.sendClipboard() { desktop.show("Clipboard is empty") }
                }
            }
            if model.displays.count > 1 {
                Menu {
                    ForEach(model.displays) { display in
                        Button("\(display.name)  \(display.width)×\(display.height)") {
                            desktop.session.selectDisplay(display.id)
                        }
                    }
                } label: {
                    Label("Display", systemImage: "rectangle.on.rectangle")
                }
            }
        }

        Section {
            Menu {
                Picker("Preset", selection: Binding(
                    get: { StreamPreset.infer(maxFps: model.maxFps, maxBitrateKbps: model.maxBitrateKbps) },
                    set: { preset in
                        guard let caps = preset.caps else { return }
                        desktop.session.maxFps = caps.maxFps
                        desktop.session.maxBitrateKbps = caps.maxBitrateKbps
                        desktop.update()
                    }
                )) {
                    ForEach(StreamPreset.allCases, id: \.self) { Text($0.label).tag($0) }
                }
                .pickerStyle(.menu)
                Picker("Frame rate", selection: Binding(get: { model.maxFps }, set: {
                    desktop.session.maxFps = $0
                    desktop.update()
                })) {
                    Text("Display maximum").tag(Int?.none)
                    ForEach(streamFrameRateOptions(displayMaximum: model.displayMaxFps), id: \.self) {
                        Text("\($0) fps").tag(Int?.some($0))
                    }
                }
                .pickerStyle(.menu)
                Picker("Bitrate", selection: Binding(get: { model.maxBitrateKbps }, set: {
                    desktop.session.maxBitrateKbps = $0
                    desktop.update()
                })) {
                    Text("Host maximum").tag(Int?.none)
                    ForEach([20_000, 10_000, 5_000], id: \.self) { Text("\($0 / 1000) Mbps").tag(Int?.some($0)) }
                }
                .pickerStyle(.menu)
                Picker("Codec", selection: Binding(get: { model.codec }, set: { desktop.setCodec($0) })) {
                    ForEach(CodecPreference.allCases, id: \.self) { Text($0.label).tag($0) }
                }
                .pickerStyle(.menu)
                Toggle("Show stats", isOn: Binding(get: { model.showStats }, set: { model.showStats = $0 }))
            } label: {
                Label("Stream", systemImage: "slider.horizontal.3")
            }
        }

        Section {
            Button("Disconnect", systemImage: "xmark.circle", role: .destructive) {
                expanded = false
                desktop.session.stop()
            }
        }
    }

    /// Collapses the controls after a few idle seconds.
    private func poke() {
        collapseTask?.cancel()
        guard expanded else { return }
        collapseTask = Task {
            try? await Task.sleep(for: .seconds(6))
            guard !Task.isCancelled else { return }
            expanded = false
        }
    }
}

struct ChromeButton: View {
    let title: String
    let systemImage: String
    let action: () -> Void

    init(_ title: String, systemImage: String, action: @escaping () -> Void) {
        self.title = title
        self.systemImage = systemImage
        self.action = action
    }

    var body: some View {
        Button(action: action) {
            Image(systemName: systemImage)
                .font(.system(size: 17, weight: .semibold))
                .frame(width: 44, height: 44)
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .glass(in: Circle())
        .accessibilityLabel(title)
    }
}

struct AirPlayButton: UIViewRepresentable {
    func makeUIView(context: Context) -> AVRoutePickerView {
        let picker = AVRoutePickerView()
        picker.prioritizesVideoDevices = true
        picker.tintColor = .white
        picker.activeTintColor = .systemBlue
        return picker
    }

    func updateUIView(_ uiView: AVRoutePickerView, context: Context) {}
}

struct StageView: UIViewRepresentable {
    let stage: Stage

    func makeUIView(context: Context) -> Stage { stage }
    func updateUIView(_ uiView: Stage, context: Context) {}
}
