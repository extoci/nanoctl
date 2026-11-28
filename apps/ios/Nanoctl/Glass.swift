import SwiftUI

/// Liquid Glass on iOS 26+, system materials before that. The glass APIs only
/// exist in the iOS 26 SDK (Swift 6.2), so older toolchains build the fallback.
extension View {
    @ViewBuilder
    func glass<S: Shape>(in shape: S, interactive: Bool = true, tint: Color? = nil) -> some View {
        #if compiler(>=6.2)
        if #available(iOS 26, *) {
            glassEffect(interactive ? .regular.tint(tint).interactive() : .regular.tint(tint), in: shape)
        } else {
            background(.ultraThinMaterial, in: shape)
                .overlay { shape.stroke(Color.white.opacity(0.11), lineWidth: 1) }
        }
        #else
        background(.ultraThinMaterial, in: shape)
            .overlay { shape.stroke(Color.white.opacity(0.11), lineWidth: 1) }
        #endif
    }

    @ViewBuilder
    func glassButton(prominent: Bool = false) -> some View {
        #if compiler(>=6.2)
        if #available(iOS 26, *) {
            if prominent { buttonStyle(.glassProminent) } else { buttonStyle(.glass) }
        } else {
            if prominent { buttonStyle(.borderedProminent) } else { buttonStyle(.bordered) }
        }
        #else
        if prominent { buttonStyle(.borderedProminent) } else { buttonStyle(.bordered) }
        #endif
    }

    @ViewBuilder
    func glassID(_ id: String, in namespace: Namespace.ID) -> some View {
        #if compiler(>=6.2)
        if #available(iOS 26, *) { glassEffectID(id, in: namespace) } else { self }
        #else
        self
        #endif
    }
}

/// Lets neighbouring glass shapes blend and morph into each other.
struct GlassGroup<Content: View>: View {
    var spacing: CGFloat = 10
    @ViewBuilder var content: Content

    var body: some View {
        #if compiler(>=6.2)
        if #available(iOS 26, *) {
            GlassEffectContainer(spacing: spacing) { content }
        } else {
            content
        }
        #else
        content
        #endif
    }
}
