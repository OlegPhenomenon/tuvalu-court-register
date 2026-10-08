import AppKit
import CoreText
import CoreGraphics
let out = CommandLine.arguments[1]
func make(_ name: String, image: Bool) {
    var box = CGRect(x: 0, y: 0, width: 400, height: 200)
    let url = URL(fileURLWithPath: out + "/" + name) as CFURL
    let ctx = CGContext(url, mediaBox: &box, [kCGPDFContextTitle as String: "DEMO fictional training fixture"] as CFDictionary)!
    ctx.beginPDFPage(nil)
    ctx.textPosition = CGPoint(x: 20, y: 150)
    let font = CTFontCreateWithName("ArialMT" as CFString, 16, nil)
    let line = CTLineCreateWithAttributedString(NSAttributedString(string: "DEMO fictional Quartz notice", attributes: [.font: font]))
    CTLineDraw(line, ctx)
    if image {
        let pixels = (0..<32*32*3).map { UInt8(($0 * 17) % 256) }
        let data = Data(pixels) as CFData
        let img = CGImage(width: 32, height: 32, bitsPerComponent: 8, bitsPerPixel: 24, bytesPerRow: 96, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGBitmapInfo(rawValue: 0), provider: CGDataProvider(data: data)!, decode: nil, shouldInterpolate: false, intent: .defaultIntent)!
        ctx.draw(img, in: CGRect(x: 20, y: 30, width: 96, height: 96))
    }
    ctx.endPDFPage()
    ctx.closePDF()
}
make("quartz-truetype.pdf", image: false)
make("quartz-flate-image.pdf", image: true)
