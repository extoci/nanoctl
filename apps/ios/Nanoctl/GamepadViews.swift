import SwiftUI

struct GamepadStatusList: View {
    let statuses: [GamepadStatus]
    var emptyMessage = "Pair a controller with this iPhone to see it here."

    var body: some View {
        if statuses.isEmpty {
            HStack(spacing: 14) {
                NanoIconBadge(systemImage: "gamecontroller", tint: .secondary, size: 40)
                VStack(alignment: .leading, spacing: 3) {
                    Text("No controller paired")
                        .font(.subheadline.weight(.semibold))
                    Text(emptyMessage)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Spacer()
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 13)
            .glass(in: RoundedRectangle(cornerRadius: 22, style: .continuous), interactive: false)
        } else {
            VStack(spacing: 8) {
                ForEach(statuses) { status in
                    GamepadStatusRow(status: status)
                }
            }
        }
    }
}

struct GamepadStatusRow: View {
    let status: GamepadStatus

    var body: some View {
        HStack(spacing: 14) {
            ZStack {
                RoundedRectangle(cornerRadius: 13, style: .continuous)
                    .fill(NanoTheme.accent.opacity(0.16))
                Text("\(status.slot)")
                    .font(.subheadline.weight(.bold).monospacedDigit())
                    .foregroundStyle(NanoTheme.accent)
            }
            .frame(width: 40, height: 40)

            VStack(alignment: .leading, spacing: 3) {
                Text(status.recognition)
                    .font(.subheadline.weight(.semibold))
                    .lineLimit(1)
                Text("Maps to \(status.hostDevice)")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Spacer(minLength: 8)
            HStack(spacing: 5) {
                Image(systemName: batteryIcon)
                    .font(.caption)
                    .foregroundStyle(batteryColor)
                Text(shortBatteryLabel)
                    .font(.caption2.weight(.medium).monospacedDigit())
                    .foregroundStyle(.secondary)
            }
            .padding(.horizontal, 8)
            .padding(.vertical, 5)
            .background(.white.opacity(0.06), in: Capsule())
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 12)
        .glass(in: RoundedRectangle(cornerRadius: 22, style: .continuous))
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(status.recognition), player \(status.slot), maps to \(status.hostDevice), \(status.batteryLabel)")
    }

    private var shortBatteryLabel: String {
        let label = status.batteryLabel.replacingOccurrences(of: "Battery ", with: "")
        return label == "unknown" ? "Unknown" : label
    }

    private var batteryIcon: String {
        guard let percent = status.batteryPercent else { return "battery.0percent" }
        if status.batteryState == "charging" { return "battery.100percent.bolt" }
        switch percent {
        case 75...100: return "battery.100percent"
        case 50..<75: return "battery.75percent"
        case 25..<50: return "battery.50percent"
        default: return "battery.25percent"
        }
    }

    private var batteryColor: Color {
        guard let percent = status.batteryPercent else { return .secondary }
        if status.batteryState == "charging" { return .green }
        return percent <= 20 ? .red : .secondary
    }
}

struct GamepadSheet: View {
    let statuses: [GamepadStatus]
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    Text("Controllers keep stable player slots, so the host sees the same virtual devices even when another controller joins or leaves.")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .padding(.horizontal, 4)
                    GamepadStatusList(statuses: statuses)
                }
                .padding(20)
            }
            .navigationTitle("Controllers")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") {
                        NanoHaptics.selection()
                        dismiss()
                    }
                }
            }
        }
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
    }
}
