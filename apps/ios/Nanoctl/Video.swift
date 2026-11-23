import CoreImage
import Metal
import UIKit
import WebRTC

final class FrameMailbox: NSObject, RTCVideoRenderer {
    private let lock = NSLock()
    private var frame: RTCVideoFrame?
    private var number = 0

    func setSize(_ size: CGSize) {}

    func renderFrame(_ frame: RTCVideoFrame?) {
        lock.lock()
        self.frame = frame
        number += 1
        lock.unlock()
    }

    func latest(after previous: Int) -> (RTCVideoFrame?, Int)? {
        lock.lock()
        defer { lock.unlock() }
        return number == previous ? nil : (frame, number)
    }
}

final class VideoView: UIView {
    override class var layerClass: AnyClass { CAMetalLayer.self }
    private var metal: CAMetalLayer { layer as! CAMetalLayer }
    private let color = CGColorSpace(name: CGColorSpace.sRGB)!
    private let queue: MTLCommandQueue?
    private let context: CIContext?
    private var displayLink: CADisplayLink?
    private var busy = false
    private var last = -1
    private var imageSize = CGSize.zero
    private var mailbox: FrameMailbox?
    #if DEBUG && targetEnvironment(simulator)
    weak var probe: LatencyProbe?
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
    }

    required init?(coder: NSCoder) { nil }

    func show(_ mailbox: FrameMailbox?) {
        if self.mailbox === mailbox { return }
        self.mailbox = mailbox
        last = -1
        imageSize = .zero
        metal.opacity = mailbox == nil ? 0 : 1
        updateLink()
    }

    override func didMoveToWindow() {
        super.didMoveToWindow()
        updateLink()
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        let scale = window?.windowScene?.screen.scale ?? traitCollection.displayScale
        metal.drawableSize = CGSize(width: (bounds.width * scale).rounded(), height: (bounds.height * scale).rounded())
        last = -1
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

    private func updateLink() {
        displayLink?.invalidate()
        displayLink = nil
        guard mailbox != nil, let screen = window?.windowScene?.screen else { return }
        let link = screen.displayLink(withTarget: self, selector: #selector(drawFrame))
        let maxRate = Float(screen.maximumFramesPerSecond)
        link?.preferredFrameRateRange = CAFrameRateRange(minimum: min(60, maxRate), maximum: maxRate, preferred: maxRate)
        link?.add(to: .main, forMode: .common)
        displayLink = link
    }

    @objc private func drawFrame(_ link: CADisplayLink) {
        guard !busy, !bounds.isEmpty, let context, let (frame, number) = mailbox?.latest(after: last),
              let drawable = metal.nextDrawable(), let command = queue?.makeCommandBuffer() else { return }
        let destination = CGRect(x: 0, y: 0, width: drawable.texture.width, height: drawable.texture.height)
        let black = CIImage(color: .black).cropped(to: destination)
        var output = black
        if let frame, let buffer = pixelBuffer(frame) {
            let image = CIImage(cvPixelBuffer: buffer)
            imageSize = image.extent.size
            let rect = fit(imageSize, in: destination)
            let scale = rect.width / imageSize.width
            output = image.transformed(by: CGAffineTransform(scaleX: scale, y: scale))
                .transformed(by: CGAffineTransform(translationX: rect.minX, y: rect.minY))
                .composited(over: black)
            #if DEBUG && targetEnvironment(simulator)
            if let probe, let marker = FrameMarker.read(buffer) {
                let width = Int(frame.width), height = Int(frame.height)
                let target = link.targetTimestamp
                let interval = max(link.duration, 0.001)
                command.addCompletedHandler { [weak probe] command in
                    guard command.status == .completed else { return }
                    let late = max(0, CACurrentMediaTime() - target)
                    let at = target + ceil(late / interval) * interval
                    DispatchQueue.main.async { probe?.record(marker, at: at, width: width, height: height) }
                }
            }
            #endif
        }
        last = number
        busy = true
        context.render(output, to: drawable.texture, commandBuffer: command, bounds: destination, colorSpace: color)
        command.addCompletedHandler { [weak self] _ in
            DispatchQueue.main.async { self?.busy = false }
        }
        command.present(drawable)
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
