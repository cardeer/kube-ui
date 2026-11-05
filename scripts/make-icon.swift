// Renders the app icon as a 1024x1024 PNG: a helm wheel on a blue rounded square.
// Built and run by scripts/bundle-macos.sh: make-icon <output.png>
import AppKit

let size: CGFloat = 1024
let output = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "icon.png"

let image = NSImage(size: NSSize(width: size, height: size))
image.lockFocus()

// macOS icon grid: ~10% inset, continuous-ish corner radius.
let inset: CGFloat = 100
let tile = NSRect(x: inset, y: inset, width: size - 2 * inset, height: size - 2 * inset)
let path = NSBezierPath(roundedRect: tile, xRadius: 185, yRadius: 185)
let gradient = NSGradient(
    starting: NSColor(red: 0.20, green: 0.45, blue: 0.95, alpha: 1),
    ending: NSColor(red: 0.10, green: 0.25, blue: 0.70, alpha: 1)
)!
gradient.draw(in: path, angle: -90)

// Generic eight-spoke ship's wheel.
let center = NSPoint(x: size / 2, y: size / 2)
NSColor.white.setStroke()
NSColor.white.setFill()
let ring = NSBezierPath()
ring.appendArc(withCenter: center, radius: 230, startAngle: 0, endAngle: 360)
ring.lineWidth = 56
ring.stroke()
for i in 0..<8 {
    let angle = CGFloat(i) * (.pi / 4)
    let spoke = NSBezierPath()
    spoke.move(to: NSPoint(x: center.x + cos(angle) * 60, y: center.y + sin(angle) * 60))
    spoke.line(to: NSPoint(x: center.x + cos(angle) * 330, y: center.y + sin(angle) * 330))
    spoke.lineWidth = 48
    spoke.lineCapStyle = .round
    spoke.stroke()
}
NSBezierPath(ovalIn: NSRect(x: center.x - 95, y: center.y - 95, width: 190, height: 190)).fill()

image.unlockFocus()

guard let tiff = image.tiffRepresentation,
      let rep = NSBitmapImageRep(data: tiff),
      let png = rep.representation(using: .png, properties: [:]) else {
    fatalError("failed to encode PNG")
}
try png.write(to: URL(fileURLWithPath: output))
