#if DEBUG && targetEnvironment(simulator)
import Foundation
import QuartzCore

struct SessionDiagnostics {
    let state: String
    let transport: String
    let codec: String?
    let inputReady: Bool
    let dcMessages: Int
    let dcFed: Int
    let dcDecoded: Int
    let dcDropped: Int
    let dcGaps: Int
    let dcKeyframeRequests: Int
    let dcFaulted: Bool
    let dcKeys: Int
    let dcExpectedSeq: UInt32?
    let dcWaitingForKey: Bool
    let dcReordered: Int
    let dcPending: Int
}

@MainActor
final class NetworkProbe {
    struct StateChange: Encodable {
        let at: Double
        let state: String
    }
    struct Sample: Encodable {
        let at: Double
        let state: String
        let transport: String
        let codec: String?
        let inputReady: Bool
        let renders: Int
        let dcMessages: Int
        let dcFed: Int
        let dcDecoded: Int
        let dcDropped: Int
        let dcGaps: Int
        let dcKeyframeRequests: Int
        let dcFaulted: Bool
        let dcKeys: Int
        let dcExpectedSeq: UInt32?
        let dcWaitingForKey: Bool
        let dcReordered: Int
        let dcPending: Int
    }
    struct Report: Encodable {
        let schemaVersion: Int
        let startedAt: Double
        let renders: [Double]
        let states: [StateChange]
        let samples: [Sample]
    }

    static let enabled = ProcessInfo.processInfo.environment["NANOCTL_NETWORK_PROBE_URL"] != nil

    private weak var session: Session?
    private let started = CACurrentMediaTime()
    private let epochOffset = Date().timeIntervalSince1970 - CACurrentMediaTime()
    private var renders: [Double] = []
    private var states: [StateChange] = []
    private var samples: [Sample] = []
    private var lastState: String?
    private var task: Task<Void, Never>?

    init(session: Session) {
        self.session = session
        task = Task { [weak self] in
            while !Task.isCancelled {
                self?.sample()
                try? await Task.sleep(for: .seconds(1))
            }
        }
    }

    deinit { task?.cancel() }

    func recordRender() {
        renders.append(CACurrentMediaTime() - started)
        if renders.count > 5000 { renders.removeFirst(renders.count - 5000) }
    }

    private func sample() {
        guard let session else { return }
        let now = CACurrentMediaTime() - started
        let diagnostics = session.diagnostics()
        if diagnostics.state != lastState {
            lastState = diagnostics.state
            states.append(StateChange(at: now, state: diagnostics.state))
        }
        samples.append(Sample(
            at: now, state: diagnostics.state, transport: diagnostics.transport, codec: diagnostics.codec,
            inputReady: diagnostics.inputReady, renders: renders.count,
            dcMessages: diagnostics.dcMessages, dcFed: diagnostics.dcFed, dcDecoded: diagnostics.dcDecoded,
            dcDropped: diagnostics.dcDropped, dcGaps: diagnostics.dcGaps,
            dcKeyframeRequests: diagnostics.dcKeyframeRequests, dcFaulted: diagnostics.dcFaulted,
            dcKeys: diagnostics.dcKeys, dcExpectedSeq: diagnostics.dcExpectedSeq,
            dcWaitingForKey: diagnostics.dcWaitingForKey, dcReordered: diagnostics.dcReordered,
            dcPending: diagnostics.dcPending
        ))
        if samples.count > 720 { samples.removeFirst(samples.count - 720) }
        write()
    }

    private func write() {
        let report = Report(schemaVersion: 1, startedAt: started + epochOffset,
                            renders: renders, states: states, samples: samples)
        guard let data = try? JSONEncoder().encode(report) else { return }
        try? data.write(to: URL.documentsDirectory.appending(path: "network-probe.json"), options: .atomic)
    }
}
#endif
