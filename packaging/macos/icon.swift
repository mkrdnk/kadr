import AppKit

// Reproduce the checked-in SVG without an unpinned image conversion dependency.
let destination = URL(fileURLWithPath: CommandLine.arguments[1])
try FileManager.default.createDirectory(at: destination, withIntermediateDirectories: true)
for size in [16, 32, 128, 256, 512] {
    for scale in [1, 2] {
        let pixels = size * scale
        let bitmap = NSBitmapImageRep(
            bitmapDataPlanes: nil, pixelsWide: pixels, pixelsHigh: pixels,
            bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true,
            isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: bitmap)
        let transform = NSAffineTransform()
        transform.scaleX(by: CGFloat(pixels) / 512, yBy: CGFloat(pixels) / 512)
        transform.concat()
        NSColor(calibratedRed: 24/255, green: 32/255, blue: 46/255, alpha: 1).setFill()
        NSBezierPath(roundedRect: NSRect(x: 32, y: 32, width: 448, height: 448),
                     xRadius: 96, yRadius: 96).fill()
        NSColor(calibratedRed: 130/255, green: 215/255, blue: 176/255, alpha: 1).setFill()
        let k = NSBezierPath()
        k.move(to: NSPoint(x: 128, y: 144))
        for point in [(192,144),(192,240),(296,144),(384,144),(264,256),
                      (384,368),(296,368),(192,272),(192,368),(128,368)] {
            k.line(to: NSPoint(x: CGFloat(point.0), y: CGFloat(point.1)))
        }
        k.close()
        k.fill()
        NSGraphicsContext.restoreGraphicsState()
        let name = "icon_\(size)x\(size)\(scale == 2 ? "@2x" : "").png"
        try bitmap.representation(using: .png, properties: [:])!
            .write(to: destination.appendingPathComponent(name))
    }
}
