import Foundation
import OSLog
import SwiftUI

/// In-memory ring buffer mirroring everything the app logs, so a user can
/// read or export debug logs on-device without a Mac attached. Entries cover
/// the current process run and survive host disconnects/reconnects.
final class AppLog: @unchecked Sendable {
    struct Entry: Sendable {
        let date: Date
        let category: String
        let level: String
        let message: String

        var line: String {
            let stamp = Self.formatter.string(from: date)
            return "\(stamp) [\(category)] \(level): \(message)"
        }

        private static let formatter: DateFormatter = {
            let formatter = DateFormatter()
            formatter.dateFormat = "HH:mm:ss.SSS"
            return formatter
        }()
    }

    static let shared = AppLog()

    private let lock = NSLock()
    private var ring: [Entry] = []
    private let capacity = 2_000

    func add(category: String, level: String, message: String) {
        lock.lock()
        if ring.count >= capacity { ring.removeFirst(ring.count - capacity + 1) }
        ring.append(Entry(date: Date(), category: category, level: level, message: message))
        lock.unlock()
    }

    func entries() -> [Entry] {
        lock.lock()
        defer { lock.unlock() }
        return ring
    }

    func text() -> String {
        entries().map(\.line).joined(separator: "\n")
    }
}

/// Drop-in replacement for `Logger` mirroring every message into `AppLog`.
/// Messages are plain strings: Swift only allows os_log privacy interpolation
/// in direct Logger calls, and this app's log content carries no secrets.
struct AppLogger: Sendable {
    let category: String
    private let logger: Logger

    init(subsystem: String, category: String) {
        self.category = category
        logger = Logger(subsystem: subsystem, category: category)
    }

    private func forward(_ level: OSLogType, _ name: String, _ message: String) {
        AppLog.shared.add(category: category, level: name, message: message)
        logger.log(level: level, "\(message, privacy: .public)")
    }

    func debug(_ message: String) { forward(.debug, "debug", message) }
    func info(_ message: String) { forward(.info, "info", message) }
    func notice(_ message: String) { forward(.default, "notice", message) }
    func warning(_ message: String) { forward(.error, "warning", message) }
    func error(_ message: String) { forward(.error, "error", message) }
}

// MARK: - Viewer

struct DebugLogView: View {
    @State private var text = ""
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            ScrollView {
                Text(text.isEmpty ? "No log entries yet." : text)
                    .font(.system(.caption2, design: .monospaced))
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding()
            }
            .navigationTitle("Debug logs")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button("Done") { dismiss() }
                }
                ToolbarItem(placement: .topBarTrailing) {
                    if !text.isEmpty {
                        ShareLink(item: exportText()) {
                            Image(systemName: "square.and.arrow.up")
                        }
                    }
                }
            }
        }
        .onAppear { text = AppLog.shared.text() }
    }

    private func exportText() -> String {
        "\(BuildInfo.versionLine)\n\n\(text)\n"
    }
}
