import CoreVideo
import Foundation

/// Direct video delivery over `nanoctl.video.v1` / `nanoctl.video-idr.v1`:
/// Annex-B access units with no jitter buffer. Mirrors
/// `attachDataChannelVideo` in the browser viewer.
///
/// Wire format: `[u32 seq][u8 flags (bit0 keyframe, bit1 fragmented,
/// bit2 timestamped, bit3 heartbeat)]`, then optional u16 part/parts and u64 sent_ms before the
/// payload chunk, all little-endian. Deltas ride the unordered
/// bounded-retransmit channel; keyframes ride the reliable IDR channel. A sequence gap
/// breaks the reference chain: deltas after the gap are dropped and the viewer
/// sends the text `KF` on the video channel until a keyframe arrives.
final class DCVideoReceiver {
    struct Stats {
        var msgs = 0
        var heartbeats = 0
        var fed = 0
        var gaps = 0
        var kfRequests = 0
        var keys = 0
        var late = 0
        var transitMs = 0.0
    }

    struct Status {
        let stats: Stats
        let codec: VTDecoder.Codec?
        let decoded: Int
        let dropped: Int
        let faulted: Bool
        let expectedSeq: UInt32?
        let waitingForKey: Bool
        let reorderedCount: Int
        let pendingCount: Int
    }

    // Match the browser viewer: unordered SCTP reorders neighboring access
    // units under load, and a frame-count window alone becomes seconds of dead
    // time after congestion adaptation lowers the frame rate.
    private let window = 16
    private let partialWindow = 16
    private let reorderAge: CFAbsoluteTime = 0.4
    private let keyframeReorderAge: CFAbsoluteTime = 10
    /// The codec SDP negotiation reported, used only when the stream itself
    /// carries no parameter sets to sniff. SDP can lie (the answer may merely
    /// mirror the offer's codec ordering); the bitstream does not.
    private let codecHint: VTDecoder.Codec?
    private var decoder: VTDecoder?
    /// The codec actually detected on the wire, once identified.
    private(set) var codec: VTDecoder.Codec?
    /// Sends the `KF` text message on the video channel.
    var requestKeyframe: () -> Void = {}
    /// Latest decoded frame.
    var onBuffer: (CVPixelBuffer) -> Void = { _ in }
    /// Fires when the stream cannot be sniffed as H.264 or HEVC (e.g. AV1
    /// negotiated while the data channels are still up) — the caller should
    /// close the channels and fall back to the RTP path.
    var onUnsupported: () -> Void = {}
    /// Repeated decoder failure even across replacement keyframes; the caller
    /// should close the channels and use the RTP video track instead.
    var onPersistentFault: () -> Void = {}

    /// Only mutated on `queue`; read through `snapshot()` from other actors.
    private var stats = Stats()
    // Link-quality counters drained by `takeLinkStats`.
    private var received = 0
    private var lost = 0
    private var undetectedKeys = 0

    private var expectedSeq: UInt32?
    private var waitingForKey = true
    private var kfRequestedAt: CFAbsoluteTime = 0
    private var pending: [UInt32: (isKey: Bool, parts: Int, got: [Data?], n: Int, firstSeen: CFAbsoluteTime)] = [:]
    private var reordered: [UInt32: (au: Data, isKey: Bool, firstSeen: CFAbsoluteTime)] = [:]
    private var newestSeq: UInt32?
    private var maintenanceTimer: DispatchSourceTimer?
    private var consecutiveFaults = 0
    private let maxConsecutiveFaults = 3
    private let queue = DispatchQueue(label: "nanoctl.dcvideo", qos: .userInteractive)

    init(codec hint: VTDecoder.Codec? = nil) {
        codecHint = hint
        let timer = DispatchSource.makeTimerSource(queue: queue)
        timer.schedule(deadline: .now() + .milliseconds(100), repeating: .milliseconds(100))
        timer.setEventHandler { [weak self] in
            guard let self else { return }
            self.expireReorderState()
            if self.waitingForKey { self.requestKeyframeThrottled() }
        }
        timer.resume()
        maintenanceTimer = timer
    }

    deinit {
        maintenanceTimer?.cancel()
    }

    /// Starts repair immediately rather than after the first delta. On an idle
    /// host there may otherwise be no frame that can expose the initial need
    /// for a keyframe.
    func begin() {
        queue.async { self.requestKeyframeThrottled() }
    }

    /// Queues a binary message from either video channel.
    func receive(_ data: Data) {
        queue.async { self.process(data) }
    }

    /// Rolling loss fraction plus the counters since the last call.
    func takeLinkStats() -> (fractionLost: Double, transitMs: Double, frames: Int) {
        var lost = 0
        var received = 0
        var transit = 0.0
        queue.sync {
            lost = self.lost
            received = self.received
            transit = stats.transitMs
            self.lost = 0
            self.received = 0
        }
        let total = lost + received
        return (total > 0 ? Double(lost) / Double(total) : 0, transit, received)
    }

    func statusSnapshot() -> Status {
        queue.sync {
            Status(stats: stats, codec: codec, decoded: decoder?.decoded ?? 0,
                   dropped: decoder?.dropped ?? 0, faulted: (decoder?.faults ?? 0) > 0,
                   expectedSeq: expectedSeq, waitingForKey: waitingForKey,
                   reorderedCount: reordered.count, pendingCount: pending.count)
        }
    }

    private func requestKeyframeThrottled() {
        guard CFAbsoluteTimeGetCurrent() - kfRequestedAt >= 0.4 else { return }
        kfRequestedAt = CFAbsoluteTimeGetCurrent()
        stats.kfRequests += 1
        requestKeyframe()
    }

    // A missing delta invalidates dependent frames until the next IDR, but
    // those dependent frames are not independent evidence of network loss.
    // Count only sequence holes inside the observed window; otherwise one lost
    // frame is amplified into a sustained congestion signal while repair waits.
    private func countMissingVideoFrames() {
        guard let expectedSeq, let newestSeq else { return }
        let distance = Int32(bitPattern: newestSeq &- expectedSeq)
        guard distance > 0 else { return }
        var missing = 0
        var seq = expectedSeq
        for _ in 0..<min(Int(distance), 256) {
            if pending[seq] == nil && reordered[seq] == nil { missing += 1 }
            seq &+= 1
        }
        lost += missing
    }

    private func noteReferenceChainLoss() {
        countMissingVideoFrames()
        waitingForKey = true
        stats.gaps += 1
        reordered.removeAll()
        requestKeyframeThrottled()
    }

    private func onDecoderFault() {
        waitingForKey = true
        reordered.removeAll()
        consecutiveFaults += 1
        requestKeyframeThrottled()
        if consecutiveFaults >= maxConsecutiveFaults {
            consecutiveFaults = 0
            onPersistentFault()
        }
    }

    private func expireReorderState() {
        let now = CFAbsoluteTimeGetCurrent()
        var expired = false
        for (seq, entry) in pending {
            // IDR fragments ride the reliable lane. A large keyframe can
            // legitimately take longer than the delta reorder window on a
            // constrained link; expiring it turns that latency into a permanent
            // black-screen/keyframe-request loop. Delta partials remain bounded
            // because they are too stale to be useful after this window.
            if entry.isKey, now - entry.firstSeen < keyframeReorderAge { continue }
            let distance = newestSeq.map { Int32(bitPattern: $0 &- seq) } ?? 0
            if distance > partialWindow || now - entry.firstSeen >= reorderAge {
                pending.removeValue(forKey: seq)
                if waitingForKey { lost += 1 }
                expired = true
            }
        }
        for (seq, entry) in reordered where now - entry.firstSeen >= reorderAge {
            reordered.removeValue(forKey: seq)
            expired = true
        }
        if expired, !waitingForKey {
            noteReferenceChainLoss()
        }
    }

    private func process(_ data: Data) {
        guard data.count >= 5 else { return }
        stats.msgs += 1
        let seq = data.withUnsafeBytes { $0.loadUnaligned(as: UInt32.self) }
        if newestSeq.map({ Int32(bitPattern: seq &- $0) > 0 }) ?? true { newestSeq = seq }
        let flags = data[4]
        let isKey = flags & 1 != 0
        if isKey { stats.keys += 1 }
        var offset = 5
        var part = 0
        var parts = 1
        if flags & 2 != 0 {
            guard data.count >= offset + 5 else { return }
            part = Int(data.withUnsafeBytes { $0.loadUnaligned(fromByteOffset: offset, as: UInt16.self) })
            parts = Int(data.withUnsafeBytes { $0.loadUnaligned(fromByteOffset: offset + 2, as: UInt16.self) })
            offset += 4
            guard part < parts else { return }
        }
        if flags & 4 != 0 {
            guard data.count >= offset + 8 else { return }
            let sentMs = data.withUnsafeBytes { $0.loadUnaligned(fromByteOffset: offset, as: UInt64.self) }
            stats.transitMs = Date().timeIntervalSince1970 * 1000 - Double(sentMs)
            offset += 8
        }
        if flags & 8 != 0 {
            stats.heartbeats += 1
            // Heartbeats carry the next live sequence as a watermark rather
            // than consuming it. A watermark ahead of the decoder means the
            // final delta before the idle period was lost even though no newer
            // frame exists to expose the gap.
            guard data.count == offset else { return }
            guard let expected = expectedSeq else { return }
            let diff = Int32(bitPattern: seq &- expected)
            if diff > 0, !waitingForKey {
                noteReferenceChainLoss()
            }
            return
        }
        guard data.count > offset else { return }
        // One throttled request is not a recovery guarantee: SCTP or the
        // host's forced-IDR cooldown can still drop/ignore it. While repair is
        // outstanding, live traffic and the maintenance timer both keep asking.
        if waitingForKey { requestKeyframeThrottled() }
        var chunk = data.subdata(in: offset..<data.count)

        if parts > 1 {
            var entry = pending[seq] ?? (isKey, parts, [Data?](repeating: nil, count: parts), 0, CFAbsoluteTimeGetCurrent())
            if part >= parts { return }
            if entry.got[part] == nil {
                entry.got[part] = chunk
                entry.n += 1
            }
            if entry.n < parts {
                pending[seq] = entry
                // Bounded repair state: loss must never turn fragmented-frame
                // bookkeeping into an hours-long memory leak. Thirty-two access
                // units is far beyond the reorder window but tiny in memory.
                if pending.count > 32 {
                    pending.removeAll(keepingCapacity: true)
                    if !waitingForKey {
                        waitingForKey = true
                        stats.gaps += 1
                        requestKeyframeThrottled()
                    }
                }
                return
            }
            pending.removeValue(forKey: seq)
            var joined = Data(capacity: entry.got.reduce(0) { $0 + ($1?.count ?? 0) })
            for piece in entry.got.compactMap({ $0 }) { joined.append(piece) }
            chunk = joined
            // Drop stale partials that can never complete.
            for key in pending.keys where Int32(bitPattern: seq &- key) > 0 {
                pending.removeValue(forKey: key)
            }
        }

        if expectedSeq == nil { expectedSeq = seq }
        let diff = Int32(bitPattern: seq &- expectedSeq!)
        // Loss feedback needs a network-arrival denominator, not a decode
        // denominator. Complete access units still count as received while the
        // decoder waits for an IDR; only late duplicates are excluded.
        if diff >= 0 { received += 1 }
        if diff < 0 {
            stats.late += 1
            return
        }
        var deliverNow = diff == 0
        if diff > 0, isKey {
            // A keyframe never depends on the missing deltas before it, so it
            // jumps the reorder window in every state. Older buffered deltas
            // are both undecodable and temporally stale once it exists.
            deliverNow = true
            for buffered in reordered.keys.filter({ Int32(bitPattern: $0 &- seq) < 0 }) {
                reordered.removeValue(forKey: buffered)
            }
        } else if diff > 0 {
            // Deltas are worth holding even while waiting for a key: the IDR
            // rides a separate reliable lane, so the first deltas encoded after
            // it can arrive before it. Discarding them creates a fresh gap
            // immediately after recovery and cascades into another request.
            reordered[seq] = (chunk, isKey, CFAbsoluteTimeGetCurrent())
            if reordered.count > window, !waitingForKey {
                noteReferenceChainLoss()
            }
        }

        if deliverNow { deliver(seq, au: chunk, isKey: isKey) }
        while let expected = expectedSeq, let entry = reordered[expected] {
            reordered.removeValue(forKey: expected)
            deliver(expected, au: entry.au, isKey: entry.isKey)
        }
    }

    private func deliver(_ seq: UInt32, au: Data, isKey: Bool) {
        if let expected = expectedSeq {
            let skipped = Int32(bitPattern: seq &- expected)
            if skipped > 0, !waitingForKey { countMissingVideoFrames() }
        }
        expectedSeq = seq &+ 1
        if waitingForKey {
            requestKeyframeThrottled()
            guard isKey else { return }
            waitingForKey = false
        }
        stats.fed += 1
        guard let decoder = ensureDecoder(au) else { return }
        decoder.decode(au, isKey: isKey, seq: seq)
    }

    /// The decoder is created from the first access unit that carries
    /// parameter sets — the codec is whatever the bitstream says it is.
    private func ensureDecoder(_ au: Data) -> VTDecoder? {
        if let decoder { return decoder }
        guard let detected = VTDecoder.detectCodec(au) else {
            undetectedKeys += 1
            if undetectedKeys > 4 {
                undetectedKeys = -1
                queue.async { [weak self] in self?.onUnsupported() }
            }
            return nil
        }
        let decoder = VTDecoder(codec: detected)
        decoder.onFault = { [weak self] in self?.queue.async { self?.onDecoderFault() } }
        decoder.onFrame = { [weak self] buffer in
            self?.onBuffer(buffer)
            self?.queue.async { self?.consecutiveFaults = 0 }
        }
        codec = detected
        self.decoder = decoder
        return decoder
    }
}
