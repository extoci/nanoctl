import SwiftUI
import UIKit

enum NanoTheme {
    static let accent = Color(red: 0.34, green: 0.68, blue: 1.0)
    static let accentSecondary = Color(red: 0.48, green: 0.42, blue: 1.0)
    static let success = Color(red: 0.24, green: 0.86, blue: 0.54)
    static let warning = Color(red: 1.0, green: 0.66, blue: 0.24)
    static let danger = Color(red: 1.0, green: 0.34, blue: 0.32)

    static let accentGradient = LinearGradient(
        colors: [accent, accentSecondary],
        startPoint: .topLeading,
        endPoint: .bottomTrailing
    )
}

enum NanoHaptics {
    static func selection() {
        UISelectionFeedbackGenerator().selectionChanged()
    }

    static func impact(_ style: UIImpactFeedbackGenerator.FeedbackStyle = .light) {
        UIImpactFeedbackGenerator(style: style).impactOccurred()
    }

    static func notify(_ type: UINotificationFeedbackGenerator.FeedbackType) {
        UINotificationFeedbackGenerator().notificationOccurred(type)
    }
}

/// Consistent physical press feedback for custom glass controls. Native glass
/// buttons already compress themselves; this is for rows and icon-only shells.
struct NanoPressButtonStyle: ButtonStyle {
    var scale: CGFloat = 0.965

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .scaleEffect(configuration.isPressed ? scale : 1)
            .opacity(configuration.isPressed ? 0.76 : 1)
            .animation(.spring(response: 0.22, dampingFraction: 0.68), value: configuration.isPressed)
    }
}

struct NanoSectionHeader: View {
    let title: String
    var trailing: String?

    init(_ title: String, trailing: String? = nil) {
        self.title = title
        self.trailing = trailing
    }

    var body: some View {
        HStack(alignment: .firstTextBaseline) {
            Text(title)
                .font(.footnote.weight(.semibold))
                .foregroundStyle(.secondary)
                .textCase(nil)
            if let trailing {
                Spacer()
                Text(trailing)
                    .font(.caption2.weight(.medium))
                    .foregroundStyle(.tertiary)
            }
        }
        .padding(.horizontal, 10)
    }
}

struct NanoStatusPill: View {
    let title: String
    let color: Color

    var body: some View {
        HStack(spacing: 5) {
            Circle()
                .fill(color)
                .frame(width: 6, height: 6)
            Text(title)
                .font(.caption2.weight(.semibold))
        }
        .foregroundStyle(color)
        .padding(.horizontal, 8)
        .padding(.vertical, 4)
        .background(color.opacity(0.13), in: Capsule())
        .accessibilityElement(children: .combine)
        .accessibilityLabel(title)
    }
}

struct ConnectBackdrop: View {
    var body: some View {
        ZStack {
            LinearGradient(colors: [Color(red: 0.025, green: 0.035, blue: 0.07), .black],
                           startPoint: .top, endPoint: .bottom)
            RadialGradient(colors: [NanoTheme.accent.opacity(0.22), .clear],
                           center: .topLeading, startRadius: 30, endRadius: 430)
            RadialGradient(colors: [NanoTheme.accentSecondary.opacity(0.16), .clear],
                           center: .bottomTrailing, startRadius: 20, endRadius: 380)
        }
        .ignoresSafeArea()
    }
}

struct NanoDisplayGlyph: View {
    var color: Color = .white

    var body: some View {
        Canvas { context, size in
            let line = size.width * 0.105
            let screenHeight = size.height * 0.61
            let screen = CGRect(x: line / 2, y: 0, width: size.width - line, height: screenHeight)
            context.stroke(Path(roundedRect: screen, cornerRadius: size.width * 0.12, style: .continuous),
                           with: .color(color), lineWidth: line)

            let stemWidth = size.width * 0.085
            let stemHeight = size.height * 0.17
            let stem = CGRect(x: (size.width - stemWidth) / 2, y: screenHeight - line * 0.15,
                              width: stemWidth, height: stemHeight)
            context.fill(Path(stem), with: .color(color))

            let footWidth = size.width * 0.52
            let footHeight = size.height * 0.105
            let foot = CGRect(x: (size.width - footWidth) / 2, y: size.height - footHeight,
                              width: footWidth, height: footHeight)
            context.fill(Path(roundedRect: foot, cornerRadius: footHeight / 2, style: .continuous), with: .color(color))
        }
        .aspectRatio(1.12, contentMode: .fit)
        .accessibilityHidden(true)
    }
}

struct NanoBrandMark: View {
    var size: CGFloat = 78

    var body: some View {
        ZStack {
            RoundedRectangle(cornerRadius: size * 0.28, style: .continuous)
                .fill(NanoTheme.accentGradient)
                .frame(width: size, height: size)
                .shadow(color: NanoTheme.accent.opacity(0.36), radius: 28, y: 12)
            NanoDisplayGlyph()
                .frame(width: size * 0.52, height: size * 0.46)
        }
        .accessibilityHidden(true)
    }
}

struct NanoIconBadge: View {
    let systemImage: String
    var tint: Color = NanoTheme.accent
    var size: CGFloat = 38

    var body: some View {
        ZStack {
            RoundedRectangle(cornerRadius: size * 0.32, style: .continuous)
                .fill(tint.opacity(0.16))
            Image(systemName: systemImage)
                .font(.system(size: size * 0.42, weight: .semibold))
                .foregroundStyle(tint)
        }
        .frame(width: size, height: size)
        .accessibilityHidden(true)
    }
}

struct NanoToast: View {
    let message: String

    private var warning: Bool {
        let text = message.lowercased()
        return text.contains("could not") || text.contains("error") || text.contains("failed") || text.contains("empty")
    }

    var body: some View {
        HStack(spacing: 9) {
            Image(systemName: warning ? "exclamationmark.triangle.fill" : "checkmark.circle.fill")
                .font(.system(size: 14, weight: .semibold))
                .foregroundStyle(warning ? NanoTheme.warning : NanoTheme.success)
            Text(message)
                .font(.callout.weight(.medium))
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(.horizontal, 17)
        .padding(.vertical, 12)
        .glass(in: Capsule(), interactive: false)
        .shadow(color: .black.opacity(0.28), radius: 18, y: 8)
        .accessibilityElement(children: .combine)
        .accessibilityLabel(message)
    }
}
