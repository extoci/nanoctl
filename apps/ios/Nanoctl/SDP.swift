import Foundation

/// SDP helpers for codec negotiation.
///
/// The bundled WebRTC build cannot express H.265 in its codec lists, so HEVC
/// negotiation is done by rewriting the local offer: an `H265` payload type is
/// injected at the front of the video m-section. The host answers with the
/// first offered codec it can encode, so first position means "preferred".
/// libwebrtc ignores the unknown payload type in the answer — the RTP stream it
/// describes is suppressed automatically once the data channels are open anyway.
enum SDP {
    /// Payload type claimed for the injected H265 codec. PTs 35 and 36 are
    /// unassigned in RTP/AVP and unused by libwebrtc's offer.
    static let hevcPayloadType = 35

    /// Returns `sdp` with an H265 payload prepended to every video m-section,
    /// or nil when no video section exists.
    static func injectHevcFirst(_ sdp: String) -> String? {
        let lines = sdp.components(separatedBy: "\r\n")
        var output: [String] = []
        output.reserveCapacity(lines.count + 8)
        var changed = false
        var inVideo = false
        var insertedAttributes = false
        for line in lines {
            if line.hasPrefix("m=") {
                if inVideo && !insertedAttributes {
                    output.append(contentsOf: hevcAttributeLines())
                    changed = true
                }
                inVideo = line.hasPrefix("m=video ")
                insertedAttributes = false
                if inVideo, let rewritten = prependPayload(line) {
                    output.append(rewritten)
                } else {
                    output.append(line)
                }
                continue
            }
            if inVideo && !insertedAttributes && line.hasPrefix("a=rtpmap:") {
                output.append(contentsOf: hevcAttributeLines())
                insertedAttributes = true
                changed = true
            }
            output.append(line)
        }
        if inVideo && !insertedAttributes {
            output.append(contentsOf: hevcAttributeLines())
            changed = true
        }
        return changed ? output.joined(separator: "\r\n") : nil
    }

    /// Removes payload type `pt` from every m=video section along with its
    /// `a=rtpmap`, `a=fmtp`, `a=rtcp-fb` and `a=rid` lines. Returns nil when a
    /// video section would be left with no payloads.
    static func removePT(_ sdp: String, _ pt: Int) -> String? {
        var output: [String] = []
        var inVideo = false
        var videoEmpty = false
        for line in sdp.components(separatedBy: "\r\n") {
            if line.hasPrefix("m=") {
                if inVideo && videoEmpty { return nil }
                inVideo = line.hasPrefix("m=video ")
                videoEmpty = false
                if inVideo {
                    var parts = line.split(separator: " ", omittingEmptySubsequences: false)
                    parts.removeAll { $0 == "\(pt)" }
                    if parts.count <= 3 { videoEmpty = true }
                    output.append(parts.joined(separator: " "))
                    continue
                }
                output.append(line)
                continue
            }
            if inVideo {
                if line.hasPrefix("a=rtpmap:\(pt) ") || line.hasPrefix("a=fmtp:\(pt) ") ||
                    line.hasPrefix("a=rtcp-fb:\(pt) ") || line.hasPrefix("a=rid:\(pt) ") {
                    continue
                }
            }
            output.append(line)
        }
        if inVideo && videoEmpty { return nil }
        return output.joined(separator: "\r\n")
    }

    /// The video codec the host picked, read from the answer's first video
    /// payload type (e.g. "h264", "hevc", "av1").
    static func negotiatedVideoCodec(_ answer: String) -> String? {
        var inVideo = false
        var firstPT: String?
        var names: [String: String] = [:]
        for line in answer.components(separatedBy: "\r\n") {
            if line.hasPrefix("m=") {
                if inVideo { break }
                if line.hasPrefix("m=video ") {
                    inVideo = true
                    let parts = line.split(separator: " ")
                    if parts.count > 3 { firstPT = String(parts[3]) }
                }
                continue
            }
            guard inVideo, line.hasPrefix("a=rtpmap:") else { continue }
            // a=rtpmap:<pt> <name>/<rate>[/...]
            let body = line.dropFirst("a=rtpmap:".count)
            guard let space = body.firstIndex(of: " ") else { continue }
            let pt = String(body[..<space])
            let name = body[body.index(after: space)...].split(separator: "/").first.map { String($0) }
            if let name { names[pt] = name.lowercased() }
        }
        guard let pt = firstPT, let name = names[pt] else { return nil }
        return name == "h265" ? "hevc" : name
    }

    private static func prependPayload(_ mLine: String) -> String? {
        // m=video <port> <proto> <pt> <pt>...
        var parts = mLine.split(separator: " ", omittingEmptySubsequences: false)
        guard parts.count > 3, parts[3] != String(hevcPayloadType) else { return nil }
        parts.insert(Substring(String(hevcPayloadType)), at: 3)
        return parts.joined(separator: " ")
    }

    private static func hevcAttributeLines() -> [String] {
        let pt = hevcPayloadType
        return [
            "a=rtpmap:\(pt) H265/90000",
            "a=rtcp-fb:\(pt) goog-remb",
            "a=rtcp-fb:\(pt) transport-cc",
            "a=rtcp-fb:\(pt) ccm fir",
            "a=rtcp-fb:\(pt) nack",
            "a=rtcp-fb:\(pt) nack pli",
            "a=fmtp:\(pt) profile-id=1",
        ]
    }
}
