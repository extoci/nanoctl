import SwiftUI

struct GamepadStatusList: View {
    let statuses: [GamepadStatus]
    var emptyMessage = "Pair a controller with this iPhone to see it here."

    var body: some View {
        if statuses.isEmpty {
            HStack(spacing: 14) {
                Image(systemName: "gamecontroller")
                    .font(.title3)
                    .foregroundStyle(.secondary)
                    .frame(width: 34)
                VStack(alignment: .leading, spacing: 3) {
                    Text("No controller paired")
                        .font(.subheadline.weight(.medium))
                    Text(emptyMessage)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Spacer()
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 12)
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
                Circle()
                    .fill(.white.opacity(0.1))
                Text("\(status.slot)")
                    .font(.caption.weight(.bold).monospacedDigit())
            }
            .frame(width: 34, height: 34)

            VStack(alignment: .leading, spacing: 3) {
                Text(status.recognition)
                    .font(.subheadline.weight(.semibold))
                    .lineLimit(1)
                Text("Maps to \(status.hostDevice) · player \(status.slot)")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Spacer(minLength: 8)
            VStack(alignment: .trailing, spacing: 3) {
                Image(systemName: batteryIcon)
                    .foregroundStyle(batteryColor)
                Text(status.batteryLabel)
                    .font(.caption2.monospacedDigit())
                    .foregroundStyle(.secondary)
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
        .glass(in: RoundedRectangle(cornerRadius: 20, style: .continuous))
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(status.recognition), player \(status.slot), maps to \(status.hostDevice), \(status.batteryLabel)")
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
                    Button("Done") { dismiss() }
                }
            }
        }
        .presentationDetents([.medium, .large])
    }
}
