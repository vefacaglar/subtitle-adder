// Draws the app icon (flat): a white body, a sound wave inside a purple speech bubble,
// with two subtitle lines below (speech → text).
//
//   swift scripts/macos/make-icon.swift <output.png> [size]
//
// `scripts/package-macos.sh` draws this at 1024 px and converts it to AppIcon.icns with iconutil.
import AppKit
import CoreGraphics

let args = CommandLine.arguments
guard args.count >= 2 else {
    FileHandle.standardError.write(Data("usage: make-icon.swift <output.png> [size]\n".utf8))
    exit(1)
}
let size = args.count >= 3 ? Double(args[2]) ?? 1024 : 1024

func color(_ hex: UInt32, _ alpha: Double = 1) -> CGColor {
    CGColor(srgbRed: Double((hex >> 16) & 0xFF) / 255, green: Double((hex >> 8) & 0xFF) / 255,
            blue: Double(hex & 0xFF) / 255, alpha: alpha)
}

let accent = color(0x5B4BDB)
let ink = color(0x1F2333)

/// The "squircle" (superellipse, n=5) body from the macOS icon grid.
func squircle(_ rect: CGRect, n: Double = 5) -> CGPath {
    let path = CGMutablePath()
    let a = rect.width / 2, b = rect.height / 2
    let steps = 720
    for i in 0...steps {
        let t = Double(i) / Double(steps) * 2 * .pi
        let c = cos(t), sn = sin(t)
        let x = rect.midX + a * (c < 0 ? -1 : 1) * pow(abs(c), 2 / n)
        let y = rect.midY + b * (sn < 0 ? -1 : 1) * pow(abs(sn), 2 / n)
        i == 0 ? path.move(to: CGPoint(x: x, y: y)) : path.addLine(to: CGPoint(x: x, y: y))
    }
    path.closeSubpath()
    return path
}

let space = CGColorSpace(name: CGColorSpace.sRGB)!
guard let ctx = CGContext(data: nil, width: Int(size), height: Int(size), bitsPerComponent: 8, bytesPerRow: 0,
                          space: space, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { exit(1) }
// All measurements are on the 1024 grid, measured from the top.
ctx.scaleBy(x: size / 1024, y: size / 1024)
ctx.translateBy(x: 0, y: 1024)
ctx.scaleBy(x: 1, y: -1)

// Body: white, with a light shadow like macOS icons (so it doesn't vanish on a light Dock).
let bodyPath = squircle(CGRect(x: 100, y: 100, width: 824, height: 824))
ctx.saveGState()
ctx.setShadow(offset: CGSize(width: 0, height: 10), blur: 24, color: color(0x000000, 0.25))
ctx.addPath(bodyPath)
ctx.setFillColor(color(0xFFFFFF))
ctx.fillPath()
ctx.restoreGState()

// Speech bubble: rounded rectangle + a tail at the bottom left.
let bubble = CGRect(x: 232, y: 232, width: 560, height: 360)
ctx.setFillColor(accent)
ctx.addPath(CGPath(roundedRect: bubble, cornerWidth: 112, cornerHeight: 112, transform: nil))
ctx.fillPath()
let tail = CGMutablePath()
tail.move(to: CGPoint(x: 318, y: 530))
tail.addLine(to: CGPoint(x: 292, y: 668))
tail.addQuadCurve(to: CGPoint(x: 448, y: 576), control: CGPoint(x: 360, y: 644))
tail.closeSubpath()
ctx.addPath(tail)
ctx.fillPath()

// Sound wave: white, centered vertical bars.
let heights: [Double] = [64, 136, 208, 120, 232, 152, 80]
let barW = 36.0, gap = 28.0
let totalW = Double(heights.count) * barW + Double(heights.count - 1) * gap
ctx.setFillColor(color(0xFFFFFF))
for (i, h) in heights.enumerated() {
    let x = bubble.midX - totalW / 2 + Double(i) * (barW + gap)
    let r = CGRect(x: x, y: bubble.midY - h / 2, width: barW, height: h)
    ctx.addPath(CGPath(roundedRect: r, cornerWidth: barW / 2, cornerHeight: barW / 2, transform: nil))
    ctx.fillPath()
}

// Subtitle lines: dark, two lines, centered.
ctx.setFillColor(ink)
for (y, w) in [(704.0, 496.0), (776.0, 320.0)] {
    let bar = CGRect(x: 512 - w / 2, y: y, width: w, height: 40)
    ctx.addPath(CGPath(roundedRect: bar, cornerWidth: 20, cornerHeight: 20, transform: nil))
    ctx.fillPath()
}

guard let image = ctx.makeImage(),
      let data = NSBitmapImageRep(cgImage: image).representation(using: .png, properties: [:]) else { exit(1) }
try data.write(to: URL(filePath: args[1]))
