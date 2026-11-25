import AVFAudio
import Foundation
import WebRTC

/// WebRTC audio device that renders remote audio through a RemoteIO output
/// unit with its input bus disabled. Unlike the default VoiceProcessingIO
/// device module, creating this unit never opens the microphone.
final class PlayoutOnlyAudioDevice: NSObject {
    private let audioSession = AVAudioSession.sharedInstance()
    private var delegate: RTCAudioDeviceDelegate?
    private var audioUnit: AUAudioUnit?
    private var outputFormat: AVAudioFormat?
    private var observers: [NSObjectProtocol] = []
    private var shouldPlay = false
    private var interrupted = false
    private var outputLatencySeconds = 0.0

    private func configureAudioSession() throws {
        try audioSession.setCategory(.playback, mode: .default, options: [.allowBluetoothA2DP])
        try audioSession.setActive(true)
    }

    private func stopHardware() {
        guard let audioUnit else { return }
        if audioUnit.isRunning {
            audioUnit.stopHardware()
            delegate?.notifyAudioOutputInterrupted()
        }
        if audioUnit.renderResourcesAllocated {
            audioUnit.deallocateRenderResources()
        }
    }

    @discardableResult
    private func updateAudioUnit() -> Bool {
        guard let audioUnit, let delegate else { return false }
        guard shouldPlay, !interrupted else {
            stopHardware()
            return true
        }
        do {
            try configureAudioSession()
        } catch {
            return false
        }

        func restartForFormatChange() {
            stopHardware()
        }
        if audioUnit.isInputEnabled {
            restartForFormatChange()
            audioUnit.isInputEnabled = false
        }
        if !audioUnit.isOutputEnabled {
            restartForFormatChange()
            audioUnit.isOutputEnabled = true
        }

        let channels = AVAudioChannelCount(max(1, min(2, audioSession.outputNumberOfChannels)))
        guard let format = AVAudioFormat(
            commonFormat: .pcmFormatInt16,
            sampleRate: audioSession.sampleRate,
            channels: channels,
            interleaved: true
        ) else { return false }
        do {
            if audioUnit.inputBusses[0].format != format {
                restartForFormatChange()
                try audioUnit.inputBusses[0].setFormat(format)
            }
        } catch {
            return false
        }
        let changed = outputFormat != format || outputLatencySeconds != audioSession.outputLatency
        outputFormat = format
        outputLatencySeconds = audioSession.outputLatency
        if changed {
            delegate.notifyAudioOutputParametersChange()
        }

        if audioUnit.outputProvider == nil {
            let getPlayoutData = delegate.getPlayoutData
            audioUnit.outputProvider = { actionFlags, timestamp, frameCount, busNumber, outputData in
                getPlayoutData(actionFlags, timestamp, busNumber, frameCount, outputData)
            }
        }

        do {
            if !audioUnit.renderResourcesAllocated {
                try audioUnit.allocateRenderResources()
            }
            if !audioUnit.isRunning {
                try audioUnit.startHardware()
            }
            return true
        } catch {
            return false
        }
    }

    private func observeAudioSession() {
        guard observers.isEmpty else { return }
        let center = NotificationCenter.default
        observers.append(center.addObserver(
            forName: AVAudioSession.interruptionNotification,
            object: audioSession,
            queue: nil
        ) { [weak self] notification in
            guard let self,
                  let raw = notification.userInfo?[AVAudioSessionInterruptionTypeKey] as? NSNumber,
                  let type = AVAudioSession.InterruptionType(rawValue: raw.uintValue) else { return }
            switch type {
            case .began:
                self.delegate?.dispatchAsync {
                    self.interrupted = true
                    self.stopHardware()
                }
            case .ended:
                self.delegate?.dispatchAsync {
                    self.interrupted = false
                    self.updateAudioUnit()
                }
            @unknown default:
                break
            }
        })
        observers.append(center.addObserver(
            forName: AVAudioSession.routeChangeNotification,
            object: audioSession,
            queue: nil
        ) { [weak self] _ in
            guard let self else { return }
            self.delegate?.dispatchAsync { self.updateAudioUnit() }
        })
        observers.append(center.addObserver(
            forName: AVAudioSession.mediaServicesWereResetNotification,
            object: audioSession,
            queue: nil
        ) { [weak self] _ in
            guard let self else { return }
            self.delegate?.dispatchAsync { self.updateAudioUnit() }
        })
    }
}

extension PlayoutOnlyAudioDevice: RTCAudioDevice {
    var deviceInputSampleRate: Double { audioSession.sampleRate }
    var inputIOBufferDuration: TimeInterval { audioSession.ioBufferDuration }
    var inputNumberOfChannels: Int { 1 }
    var inputLatency: TimeInterval { 0 }

    var deviceOutputSampleRate: Double { outputFormat?.sampleRate ?? audioSession.sampleRate }
    var outputIOBufferDuration: TimeInterval { audioSession.ioBufferDuration }
    var outputNumberOfChannels: Int {
        Int(outputFormat?.channelCount ?? AVAudioChannelCount(max(1, min(2, audioSession.outputNumberOfChannels))))
    }
    var outputLatency: TimeInterval { outputLatencySeconds }

    var isInitialized: Bool { delegate != nil && audioUnit != nil }

    func initialize(with delegate: RTCAudioDeviceDelegate) -> Bool {
        guard self.delegate == nil else { return false }
        let description = AudioComponentDescription(
            componentType: kAudioUnitType_Output,
            componentSubType: kAudioUnitSubType_RemoteIO,
            componentManufacturer: kAudioUnitManufacturer_Apple,
            componentFlags: 0,
            componentFlagsMask: 0
        )
        do {
            let audioUnit = try AUAudioUnit(componentDescription: description)
            audioUnit.isInputEnabled = false
            audioUnit.isOutputEnabled = false
            audioUnit.maximumFramesToRender = 1024
            self.audioUnit = audioUnit
            self.delegate = delegate
            observeAudioSession()
            return true
        } catch {
            return false
        }
    }

    func terminateDevice() -> Bool {
        shouldPlay = false
        stopHardware()
        audioUnit = nil
        delegate = nil
        for observer in observers { NotificationCenter.default.removeObserver(observer) }
        observers = []
        return true
    }

    var isPlayoutInitialized: Bool { isInitialized }
    func initializePlayout() -> Bool {
        guard isPlayoutInitialized else { return false }
        do {
            try configureAudioSession()
            let channels = AVAudioChannelCount(max(1, min(2, audioSession.outputNumberOfChannels)))
            outputFormat = AVAudioFormat(
                commonFormat: .pcmFormatInt16,
                sampleRate: audioSession.sampleRate,
                channels: channels,
                interleaved: true
            )
            outputLatencySeconds = audioSession.outputLatency
            return outputFormat != nil
        } catch {
            return false
        }
    }
    var isPlaying: Bool { shouldPlay }

    func startPlayout() -> Bool {
        shouldPlay = true
        return updateAudioUnit()
    }

    func stopPlayout() -> Bool {
        shouldPlay = false
        stopHardware()
        return true
    }

    var isRecordingInitialized: Bool { false }
    func initializeRecording() -> Bool { false }
    var isRecording: Bool { false }
    func startRecording() -> Bool { false }
    func stopRecording() -> Bool { true }
}
