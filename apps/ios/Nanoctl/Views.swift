import AVKit
import SwiftUI

struct RootView: View {
    let desktop: Desktop

    var body: some View {
        let model = desktop.model
        ZStack {
            if model.active {
                SessionView(desktop: desktop)
                    .transition(.opacity.combined(with: .scale(scale: 0.985)))
            } else {
                ConnectView(desktop: desktop)
                    .transition(.opacity.combined(with: .scale(scale: 1.015)))
            }
        }
        .overlay(alignment: .bottom) {
            if let toast = model.toast {
                NanoToast(message: toast)
                    .padding(24)
                    .transition(.move(edge: .bottom).combined(with: .opacity).combined(with: .scale(scale: 0.94)))
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
            ConnectBackdrop()
            ScrollView {
                VStack(spacing: 28) {
                    VStack(spacing: 14) {
                        NanoBrandMark()
                        VStack(spacing: 6) {
                            Text("nanoctl")
                                .font(.system(size: 42, weight: .bold, design: .rounded))
                                .tracking(-1.2)
                            Text("Your computers, instantly at hand.")
                                .font(.subheadline)
                                .foregroundStyle(.secondary)
                        }
                    }
                    .padding(.top, 62)

                    GlassGroup(spacing: 12) {
                        VStack(spacing: 12) {
                            HStack(spacing: 10) {
                                HStack(spacing: 11) {
                                    Image(systemName: "link")
                                        .font(.system(size: 15, weight: .semibold))
                                        .foregroundStyle(.tertiary)
                                    TextField("host.tailnet.ts.net", text: $address)
                                        .textContentType(.URL)
                                        .keyboardType(.URL)
                                        .textInputAutocapitalization(.never)
                                        .autocorrectionDisabled()
                                        .submitLabel(.go)
                                        .focused($focused)
                                        .onSubmit(connect)
                                    if !address.isEmpty {
                                        Button {
                                            address = ""
                                            NanoHaptics.selection()
                                        } label: {
                                            Image(systemName: "xmark.circle.fill")
                                                .font(.system(size: 17, weight: .medium))
                                                .foregroundStyle(.tertiary)
                                                .frame(width: 30, height: 30)
                                                .contentShape(Circle())
                                        }
                                        .buttonStyle(NanoPressButtonStyle(scale: 0.9))
                                        .accessibilityLabel("Clear host address")
                                    }
                                }
                                .padding(.horizontal, 18)
                                .frame(minHeight: 56)
                                .glass(in: Capsule())
                                Button {
                                    focused = false
                                    NanoHaptics.selection()
                                    desktop.refreshHosts(extraSeed: address)
                                } label: {
                                    Image(systemName: model.discoveringHosts ? "antenna.radiowaves.left.and.right" : "magnifyingglass")
                                        .font(.system(size: 17, weight: .semibold))
                                        .frame(width: 56, height: 56)
                                        .contentShape(Circle())
                                        .contentTransition(.symbolEffect(.replace))
                                }
                                .buttonStyle(NanoPressButtonStyle(scale: 0.92))
                                .glass(in: Circle())
                                .accessibilityLabel("Find hosts")
                            }
                            Button(action: connect) {
                                HStack(spacing: 8) {
                                    Text("Connect")
                                    Image(systemName: "arrow.right")
                                        .font(.system(size: 15, weight: .bold))
                                }
                                .font(.headline)
                                .frame(maxWidth: .infinity)
                                .frame(minHeight: 40)
                            }
                            .glassButton(prominent: true)
                            .controlSize(.large)
                            .tint(NanoTheme.accent)
                            .disabled(address.trimmingCharacters(in: .whitespaces).isEmpty)
                            .opacity(address.trimmingCharacters(in: .whitespaces).isEmpty ? 0.55 : 1)
                        }
                    }
                    .frame(maxWidth: 420)

                    if model.status != "Disconnected" {
                        Text(model.status)
                            .font(.footnote.weight(.medium))
                            .foregroundStyle(.secondary)
                            .multilineTextAlignment(.center)
                            .padding(.horizontal, 14)
                            .padding(.vertical, 8)
                            .background(.white.opacity(0.06), in: Capsule())
                    }

                    if model.discoveringHosts || !model.discoveredHosts.isEmpty || model.discoveryNotice != nil {
                        VStack(alignment: .leading, spacing: 10) {
                            HStack {
                                NanoSectionHeader("On your tailnet")
                                Spacer()
                                if model.discoveringHosts {
                                    ProgressView()
                                        .controlSize(.small)
                                        .padding(.trailing, 10)
                                } else if let notice = model.discoveryNotice {
                                    Text(notice)
                                        .font(.caption2.weight(.medium))
                                        .foregroundStyle(.tertiary)
                                        .padding(.trailing, 10)
                                }
                            }
                            if model.discoveredHosts.isEmpty, model.discoveringHosts {
                                HStack(spacing: 12) {
                                    ProgressView()
                                        .controlSize(.small)
                                        .frame(width: 34, height: 34)
                                        .background(.white.opacity(0.06), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
                                    VStack(alignment: .leading, spacing: 2) {
                                        Text("Looking for nanoctl hosts")
                                            .font(.subheadline.weight(.medium))
                                        Text("Tailnet machines appear automatically")
                                            .font(.caption)
                                            .foregroundStyle(.secondary)
                                    }
                                }
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .padding(.horizontal, 14)
                                .frame(minHeight: 64)
                                .glass(in: RoundedRectangle(cornerRadius: 22, style: .continuous), interactive: false)
                            } else if model.discoveredHosts.isEmpty {
                                HStack(spacing: 12) {
                                    NanoIconBadge(systemImage: "antenna.radiowaves.left.and.right", tint: .secondary, size: 38)
                                    VStack(alignment: .leading, spacing: 2) {
                                        Text("No hosts visible yet")
                                            .font(.subheadline.weight(.medium))
                                        Text("Enter an address or use a recent host")
                                            .font(.caption)
                                            .foregroundStyle(.secondary)
                                    }
                                }
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .padding(.horizontal, 14)
                                .frame(minHeight: 64)
                                .glass(in: RoundedRectangle(cornerRadius: 22, style: .continuous), interactive: false)
                            } else {
                                GlassGroup(spacing: 8) {
                                    VStack(spacing: 8) {
                                        ForEach(model.discoveredHosts) { host in
                                            Button {
                                                address = host.url
                                                connect()
                                            } label: {
                                                HStack(spacing: 13) {
                                                    NanoIconBadge(systemImage: hostIcon(host.os),
                                                                  tint: host.online ? NanoTheme.accent : Color.secondary,
                                                                  size: 40)
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
                                                    NanoStatusPill(title: host.online ? "Ready" : "Offline",
                                                                   color: host.online ? NanoTheme.success : .secondary)
                                                    Image(systemName: "chevron.right")
                                                        .font(.caption.weight(.semibold))
                                                        .foregroundStyle(.tertiary)
                                                }
                                                .padding(.horizontal, 14)
                                                .frame(minHeight: 66)
                                                .contentShape(RoundedRectangle(cornerRadius: 24, style: .continuous))
                                            }
                                            .buttonStyle(NanoPressButtonStyle())
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
                            NanoSectionHeader("Recent")
                            GlassGroup(spacing: 8) {
                                VStack(spacing: 8) {
                                    ForEach(model.recents, id: \.self) { host in
                                        Button { address = host; connect() } label: {
                                            HStack(spacing: 12) {
                                                NanoIconBadge(systemImage: "clock.arrow.circlepath", tint: .secondary, size: 34)
                                                Text(Self.label(host))
                                                    .font(.subheadline.weight(.medium))
                                                    .lineLimit(1)
                                                Spacer()
                                                if URL(string: host)?.scheme == "http" {
                                                    Text("HTTP")
                                                        .font(.caption2.weight(.semibold))
                                                        .foregroundStyle(.secondary)
                                                        .padding(.horizontal, 7)
                                                        .padding(.vertical, 3)
                                                        .background(.white.opacity(0.08), in: Capsule())
                                                }
                                                Image(systemName: "arrow.right")
                                                    .font(.caption.weight(.semibold))
                                                    .foregroundStyle(.tertiary)
                                            }
                                            .padding(.horizontal, 14)
                                            .frame(minHeight: 56)
                                            .contentShape(RoundedRectangle(cornerRadius: 22, style: .continuous))
                                        }
                                        .buttonStyle(NanoPressButtonStyle())
                                        .glass(in: RoundedRectangle(cornerRadius: 22, style: .continuous))
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
                        NanoSectionHeader("Controllers")
                        GamepadStatusList(statuses: model.gamepads)
                    }
                    .frame(maxWidth: 420)

                    HStack(spacing: 12) {
                        Text(BuildInfo.versionLine)
                            .font(.caption2.monospaced())
                            .foregroundStyle(.tertiary)
                            .lineLimit(1)
                        Spacer()
                        Button {
                            NanoHaptics.selection()
                            showLogs = true
                        } label: {
                            Label("Logs", systemImage: "doc.text.magnifyingglass")
                                .font(.caption.weight(.semibold))
                                .padding(.horizontal, 11)
                                .padding(.vertical, 7)
                                .contentShape(Capsule())
                        }
                        .buttonStyle(NanoPressButtonStyle(scale: 0.94))
                        .background(.white.opacity(0.07), in: Capsule())
                        .foregroundStyle(.secondary)
                    }
                    .frame(maxWidth: 420)
                    .padding(.bottom, 10)
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
        .animation(.smooth(duration: 0.28), value: model.discoveredHosts)
        .animation(.smooth(duration: 0.28), value: model.discoveringHosts)
        .animation(.smooth(duration: 0.24), value: address.isEmpty)
    }

    private func connect() {
        focused = false
        NanoHaptics.impact(.medium)
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
        VStack(spacing: 15) {
            HStack(spacing: 0) {
                ForEach(stages, id: \.self) { stage in
                    step(stage)
                    if stage != .media {
                        Capsule()
                            .fill(stage.rawValue < progress.stage.rawValue ? NanoTheme.success : Color.white.opacity(0.14))
                            .frame(width: 16, height: 2)
                            .padding(.bottom, 19)
                    }
                }
            }
            Text(progress.detail)
                .font(.subheadline.weight(.medium))
                .foregroundStyle(progress.failure == nil ? .secondary : NanoTheme.warning)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
                .id(progress.detail)
                .transition(.opacity)
            if let hint = progress.failureHint {
                Label(hint, systemImage: "lightbulb")
                    .font(.caption.weight(.medium))
                    .foregroundStyle(NanoTheme.warning)
                    .multilineTextAlignment(.center)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 8)
                    .background(NanoTheme.warning.opacity(0.1), in: Capsule())
            }
        }
        .animation(.smooth(duration: 0.25), value: progress)
    }

    private func step(_ stage: ConnectStage) -> some View {
        let complete = progress.stage == .live || stage.rawValue < progress.stage.rawValue
        let current = stage.rawValue == progress.stage.rawValue
        let failed = current && progress.failure != nil
        let color: Color = complete ? NanoTheme.success : failed ? NanoTheme.warning : current ? NanoTheme.accent : Color.white.opacity(0.28)
        return VStack(spacing: 6) {
            ZStack {
                Circle()
                    .fill(color.opacity(current || complete ? 0.18 : 0.07))
                    .overlay {
                        Circle()
                            .strokeBorder(color.opacity(current || complete ? 0.85 : 0.35), lineWidth: current ? 1.6 : 1)
                    }
                Image(systemName: complete ? "checkmark" : failed ? "exclamationmark" : icon(stage))
                    .font(.system(size: 12, weight: .bold))
                    .foregroundStyle(color)
                    .contentTransition(.symbolEffect(.replace))
            }
            .frame(width: 31, height: 31)
            .scaleEffect(current && !failed ? 1.08 : 1)
            Text(stage.label)
                .font(.caption2.weight(current ? .semibold : .medium))
                .foregroundStyle(current || complete ? Color.white : Color.white.opacity(0.45))
        }
        .frame(width: 64)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(stage.label), \(failed ? "failed" : complete ? "complete" : current ? "in progress" : "pending")")
    }

    private func icon(_ stage: ConnectStage) -> String {
        switch stage {
        case .signaling: return "antenna.radiowaves.left.and.right"
        case .networking: return "network"
        case .media: return "play.fill"
        case .live: return "checkmark"
        }
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
                VStack(spacing: 12) {
                    NanoIconBadge(systemImage: "tv", tint: NanoTheme.accentSecondary, size: 58)
                    Text("Showing on your TV")
                        .font(.headline)
                    Text("Use this screen as a trackpad.")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                }
                .padding(28)
                .glass(in: RoundedRectangle(cornerRadius: 30, style: .continuous), interactive: false)
                .foregroundStyle(.white.opacity(0.82))
                .allowsHitTesting(false)
            } else if model.connectProgress.stage != .live {
                VStack(spacing: 18) {
                    HStack(spacing: 10) {
                        NanoIconBadge(systemImage: "desktopcomputer", size: 42)
                        VStack(alignment: .leading, spacing: 3) {
                            Text(model.hostName ?? "nanoctl")
                                .font(.headline)
                                .lineLimit(1)
                            HStack(spacing: 6) {
                                ProgressView()
                                    .controlSize(.mini)
                                Text(model.connectProgress.failure == nil ? "Preparing your desktop" : "Retrying connection")
                                    .font(.caption.weight(.medium))
                                    .foregroundStyle(.secondary)
                            }
                        }
                        Spacer(minLength: 0)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    ConnectProgressView(progress: model.connectProgress)
                    Button {
                        NanoHaptics.selection()
                        desktop.session.stop()
                    } label: {
                        Text("Cancel")
                            .font(.subheadline.weight(.semibold))
                            .frame(minWidth: 118)
                    }
                    .glassButton()
                    .controlSize(.large)
                }
                .padding(24)
                .frame(maxWidth: 342)
                .glass(in: RoundedRectangle(cornerRadius: 34, style: .continuous), interactive: false)
                .shadow(color: .black.opacity(0.34), radius: 30, y: 14)
            }
        }
        .overlay(alignment: .bottomLeading) {
            if model.showStats, !model.stats.isEmpty {
                Text(model.stats)
                    .font(.caption.monospaced().weight(.medium))
                    .multilineTextAlignment(.leading)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 9)
                    .glass(in: RoundedRectangle(cornerRadius: 16, style: .continuous), interactive: false)
                    .shadow(color: .black.opacity(0.22), radius: 12, y: 5)
                    .padding(14)
                    .allowsHitTesting(false)
            }
        }
        .overlay(alignment: .topTrailing) {
            chrome(model)
                .padding(.top, 14)
                .padding(.trailing, 14)
        }
        .statusBarHidden()
        .persistentSystemOverlays(.hidden)
        .defersSystemGestures(on: .all)
        .animation(.spring(response: 0.38, dampingFraction: 0.78), value: expanded)
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
                            let next: TouchMode = model.touchMode == .trackpad ? .direct : .trackpad
                            desktop.setTouchMode(next)
                            desktop.show(next == .trackpad
                                         ? "Trackpad: drag to move, tap to click"
                                         : "Direct touch: tap where you want to click")
                        }
                        poke()
                    }
                    .glassID("touch", in: glass)

                    ChromeButton(model.keyboardVisible ? "Hide keyboard" : "Keyboard",
                                 systemImage: model.keyboardVisible ? "keyboard.chevron.compact.down" : "keyboard",
                                 active: model.keyboardVisible) {
                        desktop.setKeyboardVisible(!model.keyboardVisible)
                        poke()
                    }
                    .opacity(model.keyboardVisible ? 1 : 0.92)
                    .glassID("keyboard", in: glass)

                    ChromeButton("Controllers", systemImage: "gamecontroller.fill",
                                 active: !model.gamepads.isEmpty) {
                        showControllers = true
                        poke()
                    }
                    .accessibilityHint(model.gamepads.isEmpty
                                       ? "Shows controller type, player slot, host device, and battery"
                                       : "\(model.gamepads.count) connected. Shows controller type, player slot, host device, and battery")
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
    let active: Bool
    let action: () -> Void

    init(_ title: String, systemImage: String, active: Bool = false, action: @escaping () -> Void) {
        self.title = title
        self.systemImage = systemImage
        self.active = active
        self.action = action
    }

    var body: some View {
        Button {
            NanoHaptics.selection()
            action()
        } label: {
            Image(systemName: systemImage)
                .font(.system(size: 17, weight: .semibold))
                .frame(width: 44, height: 44)
                .contentShape(Circle())
                .contentTransition(.symbolEffect(.replace))
        }
        .buttonStyle(NanoPressButtonStyle(scale: 0.9))
        .glass(in: Circle(), tint: active ? NanoTheme.accent : nil)
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
