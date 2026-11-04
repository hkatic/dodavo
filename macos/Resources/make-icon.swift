// Draws the app icon: the blue square and white arrow of res/app.ico, as a
// drawing, on Apple's grid for Mac icons (824 of 1024, with room for its shadow).
//
//   swift macos/Resources/make-icon.swift macos/Resources/Dodavo.icns
import AppKit

func render(_ size: Int) -> Data {
    let rep = NSBitmapImageRep(
        bitmapDataPlanes: nil, pixelsWide: size, pixelsHigh: size, bitsPerSample: 8,
        samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
        bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    let scale = CGFloat(size) / 1024
    let context = NSGraphicsContext.current!.cgContext
    context.scaleBy(x: scale, y: scale)

    let square = NSRect(x: 100, y: 100, width: 824, height: 824)
    let shape = NSBezierPath(roundedRect: square, xRadius: 185, yRadius: 185)
    context.saveGState()
    context.setShadow(
        offset: CGSize(width: 0, height: -12), blur: 28,
        color: NSColor.black.withAlphaComponent(0.3).cgColor)
    NSColor.black.setFill()
    shape.fill()
    context.restoreGState()

    let top = NSColor(srgbRed: 0.145, green: 0.455, blue: 0.890, alpha: 1)
    let bottom = NSColor(srgbRed: 0.235, green: 0.600, blue: 0.980, alpha: 1)
    NSGradient(starting: bottom, ending: top)!.draw(in: shape, angle: 90)

    // The arrow.
    let arrow = NSBezierPath()
    arrow.move(to: NSPoint(x: 232, y: 424))
    arrow.line(to: NSPoint(x: 520, y: 424))
    arrow.line(to: NSPoint(x: 520, y: 268))
    arrow.line(to: NSPoint(x: 806, y: 512))
    arrow.line(to: NSPoint(x: 520, y: 756))
    arrow.line(to: NSPoint(x: 520, y: 600))
    arrow.line(to: NSPoint(x: 232, y: 600))
    arrow.close()
    NSColor.white.setFill()
    arrow.fill()

    NSGraphicsContext.restoreGraphicsState()
    return rep.representation(using: .png, properties: [:])!
}

let output = CommandLine.arguments[1]
let set = NSTemporaryDirectory() + "Dodavo-\(UUID().uuidString).iconset"
try! FileManager.default.createDirectory(atPath: set, withIntermediateDirectories: true)
for (points, scale) in [(16, 1), (16, 2), (32, 1), (32, 2), (128, 1), (128, 2), (256, 1), (256, 2), (512, 1), (512, 2)] {
    let name = "icon_\(points)x\(points)\(scale == 2 ? "@2x" : "").png"
    try! render(points * scale).write(to: URL(fileURLWithPath: set + "/" + name))
}
let process = Process()
process.executableURL = URL(fileURLWithPath: "/usr/bin/iconutil")
process.arguments = ["-c", "icns", set, "-o", output]
try! process.run()
process.waitUntilExit()
try? FileManager.default.removeItem(atPath: set)
exit(process.terminationStatus)
