// Regenerates tests/fixtures/ocr/ledger.png, the text image used by the OCR tests.
// Usage: swift scripts/generate-ocr-fixture.swift tests/fixtures/ocr/ledger.png
import AppKit

let arguments = CommandLine.arguments
guard arguments.count == 2 else {
    FileHandle.standardError.write(Data("usage: generate-ocr-fixture.swift <output.png>\n".utf8))
    exit(2)
}

let width = 1200
let height = 120
guard let context = CGContext(
    data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
    space: CGColorSpaceCreateDeviceGray(), bitmapInfo: CGImageAlphaInfo.none.rawValue
), let font = NSFont(name: "Helvetica", size: 40) else {
    exit(1)
}
context.setFillColor(gray: 1, alpha: 1)
context.fill(CGRect(x: 0, y: 0, width: width, height: height))
NSGraphicsContext.current = NSGraphicsContext(cgContext: context, flipped: false)
NSAttributedString(
    string: "fast-docs OCR fixture: Quantum Ledger Reconciliation",
    attributes: [.font: font, .foregroundColor: NSColor.black]
).draw(at: NSPoint(x: 20, y: 40))

let output = URL(fileURLWithPath: arguments[1])
guard let image = context.makeImage(),
      let destination = CGImageDestinationCreateWithURL(output as CFURL, "public.png" as CFString, 1, nil) else {
    exit(1)
}
CGImageDestinationAddImage(destination, image, nil)
guard CGImageDestinationFinalize(destination) else { exit(1) }
