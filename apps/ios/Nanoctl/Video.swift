import CoreImage
import Metal
import UIKit
import WebRTC

final class FrameMailbox: NSObject, RTCVideoRenderer {
    struct Entry {
        let frame: RTCVideoFrame?
        let number: Int
        let receivedAt: CFTimeInterval
    }

    private let lock = NSLock()
    private var entry = Entry(frame: nil, number: 0, receivedAt: 0)
    private var wakeup: DispatchSourceUserDataAdd?

    func setSize(_ size: CGSize) {}

    func renderFrame(_ frame: RTCVideoFrame?) {
        let now = CACurrentMediaTime()
        lock.lock()
        entry = Entry(frame: frame, number: entry.number + 1, receivedAt: now)
        let source = wakeup
        lock.unlock()
        source?.add(data: 1)
    }

    func setWakeup(_ source: DispatchSourceUserDataAdd?) {
        lock.lock()
        wakeup = source
        lock.unlock()
    }

    func latest(after previous: Int) -> Entry? {
        lock.lock()
        defer { lock.unlock() }
        return entry.number == previous ? nil : entry
    }
}

final class VideoView: UIView {
    override class var layerClass: AnyClass { CAMetalLayer.self }
    private var metal: CAMetalLayer { layer as! CAMetalLayer }
    private let color = CGColorSpace(name: CGColorSpace.sRGB)!
    private let queue: MTLCommandQueue?
    private let context: CIContext?
    private let wakeup = DispatchSource.makeUserDataAddSource(queue: .main)
    private var busy = false
    private var last = -1
    private var imageSize = CGSize.zero
    private var mailbox: FrameMailbox?
    #if DEBUG && targetEnvironment(simulator)
    weak var probe: LatencyProbe?
    private var displayLink: CADisplayLink?
    private var refresh: (target: CFTimeInterval, interval: CFTimeInterval)?
    #endif

    init() {
        let device = MTLCreateSystemDefaultDevice()
        queue = device?.makeCommandQueue()
        context = device.map { CIContext(mtlDevice: $0, options: [.cacheIntermediates: false]) }
        super.init(frame: .zero)
        metal.device = device
        metal.pixelFormat = .bgra8Unorm
        metal.colorspace = color
        metal.framebufferOnly = false
        metal.maximumDrawableCount = 2
        backgroundColor = .black
        wakeup.setEventHandler { [weak self] in self?.drawFrame() }
        wakeup.activate()
    }

    required init?(coder: NSCoder) { nil }

    deinit { wakeup.cancel() }

    func show(_ mailbox: FrameMailbox?) {
        if self.mailbox === mailbox { return }
        self.mailbox?.setWakeup(nil)
        self.mailbox = mailbox
        last = -1
        imageSize = .zero
        metal.opacity = mailbox == nil ? 0 : 1
        #if DEBUG && targetEnvironment(simulator)
        updateLink()
        if probe?.vsyncBaseline == true { return }
        #endif
        mailbox?.setWakeup(wakeup)
        wakeup.add(data: 1)
    }

    override func didMoveToWindow() {
        super.didMoveToWindow()
        #if DEBUG && targetEnvironment(simulator)
        updateLink()
        #endif
        wakeup.add(data: 1)
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        let scale = window?.windowScene?.screen.scale ?? traitCollection.displayScale
        metal.drawableSize = CGSize(width: (bounds.width * scale).rounded(), height: (bounds.height * scale).rounded())
        last = -1
        wakeup.add(data: 1)
    }

    private func fit(_ image: CGSize, in bounds: CGRect) -> CGRect {
        let scale = min(bounds.width / image.width, bounds.height / image.height)
        let size = CGSize(width: image.width * scale, height: image.height * scale)
        return CGRect(x: bounds.midX - size.width / 2, y: bounds.midY - size.height / 2,
                      width: size.width, height: size.height)
    }

    var inputArea: CGRect {
        imageSize.width > 0 && imageSize.height > 0 ? fit(imageSize, in: bounds) : bounds
    }

    #if DEBUG && targetEnvironment(simulator)
    private func updateLink() {
        displayLink?.invalidate()
        displayLink = nil
        refresh = nil
        guard probe != nil, mailbox != nil, let screen = window?.windowScene?.screen else { return }
        let link = screen.displayLink(withTarget: self, selector: #selector(refreshTick))
        let maxRate = Float(screen.maximumFramesPerSecond)
        link?.preferredFrameRateRange = CAFrameRateRange(minimum: min(60, maxRate), maximum: maxRate, preferred: maxRate)
        link?.add(to: .main, forMode: .common)
        displayLink = link
    }

    @objc private func refreshTick(_ link: CADisplayLink) {
        refresh = (link.targetTimestamp, max(link.duration, 0.001))
        if probe?.vsyncBaseline == true { drawFrame(fromRefresh: true) }
    }
    #endif

    private func drawFrame(fromRefresh: Bool = false) {
        #if DEBUG && targetEnvironment(simulator)
        if probe?.vsyncBaseline == true && !fromRefresh { return }
        let startedAt = CACurrentMediaTime()
        #endif
        guard !busy, window != nil, !bounds.isEmpty, let context, let entry = mailbox?.latest(after: last),
              let drawable = metal.nextDrawable(), let command = queue?.makeCommandBuffer() else { return }
        let destination = CGRect(x: 0, y: 0, width: drawable.texture.width, height: drawable.texture.height)
        let black = CIImage(color: .black).cropped(to: destination)
        var output = black
        #if DEBUG && targetEnvironment(simulator)
        var sample: (FrameMarker, Int, Int, UInt32)?
        #endif
        if let frame = entry.frame, let buffer = pixelBuffer(frame) {
            let image = CIImage(cvPixelBuffer: buffer)
            imageSize = image.extent.size
            let rect = fit(imageSize, in: destination)
            let scale = rect.width / imageSize.width
            output = image.transformed(by: CGAffineTransform(scaleX: scale, y: scale))
                .transformed(by: CGAffineTransform(translationX: rect.minX, y: rect.minY))
                .composited(over: black)
            #if DEBUG && targetEnvironment(simulator)
            if probe != nil, let marker = FrameMarker.read(buffer) {
                sample = (marker, Int(frame.width), Int(frame.height), UInt32(bitPattern: frame.timeStamp))
            }
            #endif
        }
        last = entry.number
        busy = true
        context.render(output, to: drawable.texture, commandBuffer: command, bounds: destination, colorSpace: color)
        command.present(drawable)
        #if DEBUG && targetEnvironment(simulator)
        if let probe, let (marker, width, height, rtp) = sample, let refresh {
            let submittedAt = CACurrentMediaTime()
            let decoder = DecoderTimings.shared.timing(for: rtp)
            let receivedAt = entry.receivedAt
            command.addCompletedHandler { [weak probe] command in
                guard command.status == .completed else { return }
                let completedAt = CACurrentMediaTime()
                let ticks = ceil((completedAt - refresh.target) / refresh.interval)
                let at = refresh.target + ticks * refresh.interval
                let stages = LatencyProbe.Stages(rtp: rtp, decoder: decoder, mailbox: receivedAt,
                                                renderStart: startedAt, submit: submittedAt, gpuEnd: completedAt)
                DispatchQueue.main.async {
                    probe?.record(marker, at: at, width: width, height: height, stages: stages)
                }
            }
        }
        #endif
        command.addCompletedHandler { [weak self] _ in
            DispatchQueue.main.async {
                guard let self else { return }
                self.busy = false
                #if DEBUG && targetEnvironment(simulator)
                if self.probe?.vsyncBaseline == true { return }
                #endif
                if self.mailbox?.latest(after: self.last) != nil { self.wakeup.add(data: 1) }
            }
        }
        command.commit()
    }

    private func pixelBuffer(_ frame: RTCVideoFrame) -> CVPixelBuffer? {
        if let buffer = frame.buffer as? RTCCVPixelBuffer { return buffer.pixelBuffer }
        let source = frame.buffer.toI420()
        let width = Int(source.width), height = Int(source.height)
        var buffer: CVPixelBuffer?
        guard CVPixelBufferCreate(kCFAllocatorDefault, width, height, kCVPixelFormatType_420YpCbCr8Planar,
                                  [kCVPixelBufferIOSurfacePropertiesKey: [:]] as CFDictionary,
                                  &buffer) == kCVReturnSuccess, let buffer else { return nil }
        guard CVPixelBufferLockBaseAddress(buffer, []) == kCVReturnSuccess else { return nil }
        defer { CVPixelBufferUnlockBaseAddress(buffer, []) }
        let planes = [(source.dataY, source.strideY), (source.dataU, source.strideU), (source.dataV, source.strideV)]
        for (plane, (base, stride)) in planes.enumerated() {
            guard let dest = CVPixelBufferGetBaseAddressOfPlane(buffer, plane) else { return nil }
            let outStride = CVPixelBufferGetBytesPerRowOfPlane(buffer, plane)
            for y in 0..<CVPixelBufferGetHeightOfPlane(buffer, plane) {
                dest.advanced(by: y * outStride).copyMemory(from: base.advanced(by: y * Int(stride)),
                                                          byteCount: CVPixelBufferGetWidthOfPlane(buffer, plane))
            }
        }
        return buffer
    }
}
