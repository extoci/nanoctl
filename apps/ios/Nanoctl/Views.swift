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
    }

    private func connect() {
        focused = false
        desktop.connect(address)
    }
}

// MARK: - Session

/// The desktop fills the whole screen; controls live in one glass button
/// that expands on tap and gets out of the way otherwise.
struct SessionView: View {
    let desktop: Desktop
    @State private var expanded = false
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
            } else if !model.hasVideo {
                VStack(spacing: 16) {
                    ProgressView()
                        .controlSize(.large)
                    VStack(spacing: 4) {
                        Text(model.hostName ?? "nanoctl")
                            .font(.headline)
                        Text(model.status)
                            .font(.subheadline)
                            .foregroundStyle(.secondary)
                            .multilineTextAlignment(.center)
                    }
                    Button("Cancel") { desktop.session.stop() }
                        .glassButton()
                }
                .padding(28)
                .frame(maxWidth: 320)
                .glass(in: RoundedRectangle(cornerRadius: 32, style: .continuous), interactive: false)
            }
        }
        .overlay(alignment: .topLeading) {
            if model.showStats, !model.stats.isEmpty {
                Text(model.stats)
                    .font(.caption.monospaced())
                    .padding(.horizontal, 12)
                    .padding(.vertical, 6)
                    .glass(in: Capsule(), interactive: false)
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

        Section {
            if model.audioAvailable {
                Toggle(isOn: Binding(get: { model.audioEnabled }, set: {
                    desktop.session.audioEnabled = $0
                    desktop.update()
                })) {
                    Label("Sound", systemImage: "speaker.wave.2")
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
                Picker("Frame rate", selection: Binding(get: { model.maxFps }, set: {
                    desktop.session.maxFps = $0
                    desktop.update()
                })) {
                    Text("Host maximum").tag(Int?.none)
                    ForEach([60, 30], id: \.self) { Text("\($0) fps").tag(Int?.some($0)) }
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
