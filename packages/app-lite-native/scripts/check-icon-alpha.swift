import CoreGraphics
import Foundation
import ImageIO

guard CommandLine.arguments.count == 2 else {
    fputs("check-icon-alpha: expected one PNG path\n", stderr)
    exit(2)
}

let path = CommandLine.arguments[1]
guard let source = CGImageSourceCreateWithURL(URL(fileURLWithPath: path) as CFURL, nil),
      let image = CGImageSourceCreateImageAtIndex(source, 0, nil),
      let context = CGContext(
          data: nil,
          width: image.width,
          height: image.height,
          bitsPerComponent: 8,
          bytesPerRow: image.width * 4,
          space: CGColorSpaceCreateDeviceRGB(),
          bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
      ),
      let data = context.data
else {
    fputs("check-icon-alpha: cannot decode \(path)\n", stderr)
    exit(1)
}

context.draw(image, in: CGRect(x: 0, y: 0, width: image.width, height: image.height))
let pixels = data.assumingMemoryBound(to: UInt8.self)
let corners = [(0, 0), (image.width - 1, 0), (0, image.height - 1), (image.width - 1, image.height - 1)]
for (x, y) in corners {
    let alpha = pixels[y * context.bytesPerRow + x * 4 + 3]
    guard alpha == 0 else {
        fputs("check-icon-alpha: corner alpha is \(alpha), expected 0 in \(path)\n", stderr)
        exit(1)
    }
}
