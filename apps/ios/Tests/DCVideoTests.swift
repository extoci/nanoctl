import XCTest
@testable import Nanoctl

final class DCVideoTests: XCTestCase {
    private func message(_ seq: UInt32, keyframe: Bool = false, payload: [UInt8] = [1]) -> Data {
        var data = Data()
        var wireSeq = seq.littleEndian
        withUnsafeBytes(of: &wireSeq) { data.append(contentsOf: $0) }
        data.append(keyframe ? 1 : 0)
        data.append(contentsOf: payload)
        return data
    }

    private func fragment(_ seq: UInt32, keyframe: Bool, part: UInt16, parts: UInt16, payload: [UInt8]) -> Data {
        var data = Data()
        var wireSeq = seq.littleEndian
        var wirePart = part.littleEndian
        var wireParts = parts.littleEndian
        withUnsafeBytes(of: &wireSeq) { data.append(contentsOf: $0) }
        data.append((keyframe ? 1 : 0) | 2)
        withUnsafeBytes(of: &wirePart) { data.append(contentsOf: $0) }
        withUnsafeBytes(of: &wireParts) { data.append(contentsOf: $0) }
        data.append(contentsOf: payload)
        return data
    }

    func testReorderWindowOverflowRequestsAReplacementKeyframe() {
        let receiver = DCVideoReceiver()
        let lock = NSLock()
        var requests = 0
        let requested = expectation(description: "replacement keyframe requested")
        receiver.requestKeyframe = {
            lock.lock()
            requests += 1
            let count = requests
            lock.unlock()
            if count == 2 { requested.fulfill() }
        }

        // The first keyframe is accepted from the initial waiting-for-key state;
        // arbitrary bytes are enough here because this test exercises sequence
        // repair before codec detection, not VideoToolbox decoding.
        receiver.receive(message(0, keyframe: true, payload: [0, 0, 0, 1, 0x67]))
        for seq in UInt32(2)...UInt32(18) {
            receiver.receive(message(seq, payload: [0, 0, 0, 1, 0x41]))
        }
        Thread.sleep(forTimeInterval: 0.3)
        receiver.receive(message(19, payload: [0, 0, 0, 1, 0x41]))
        wait(for: [requested], timeout: 1)
        let status = receiver.statusSnapshot()
        XCTAssertGreaterThanOrEqual(status.stats.gaps, 1)
    }

    func testIncompleteKeyframeSurvivesDeltaReorderWindow() {
        let receiver = DCVideoReceiver()
        receiver.receive(fragment(0, keyframe: true, part: 0, parts: 2, payload: [0, 0, 0, 1, 0x67]))
        for seq in UInt32(1)...UInt32(24) {
            receiver.receive(message(seq, payload: [0, 0, 0, 1, 0x41]))
        }
        Thread.sleep(forTimeInterval: 0.55)
        let status = receiver.statusSnapshot()
        XCTAssertEqual(status.pendingCount, 1, "reliable IDR reassembly must outlive the lossy delta reorder window")
    }

    func testReferenceChainLossIsNotAmplifiedIntoEveryWaitingFrame() {
        let receiver = DCVideoReceiver()
        receiver.receive(message(0, keyframe: true, payload: [0, 0, 0, 1, 0x67]))
        for seq in UInt32(2)...UInt32(19) {
            receiver.receive(message(seq, payload: [0, 0, 0, 1, 0x41]))
        }
        Thread.sleep(forTimeInterval: 0.2)
        let stats = receiver.takeLinkStats()
        XCTAssertLessThan(stats.fractionLost, 0.2, "one missing delta must not become a sustained majority-loss report")
    }
}
