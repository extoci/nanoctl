import Foundation

/// Reads the commit hash and build time the `Stamp build info` post-build
/// script writes into the app bundle's Info.plist. Falls back to "dev" for
/// builds outside a git checkout.
enum BuildInfo {
    static let commit = Bundle.main.object(forInfoDictionaryKey: "NanoctlCommit") as? String ?? "dev"
    static let builtAt = Bundle.main.object(forInfoDictionaryKey: "NanoctlBuiltAt") as? String ?? ""

    static var versionLine: String {
        let short = String(commit.prefix(7))
        return builtAt.isEmpty ? "nanoctl \(short)" : "nanoctl \(short) · \(builtAt)"
    }
}
