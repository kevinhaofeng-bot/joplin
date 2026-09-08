import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

enum MaskIconError: Error {
    case usage
    case invalidSize
    case cannotReadSource
    case cannotCreateContext
    case cannotCreateImage
    case cannotWriteOutput
}

func squirclePath(size: CGFloat) -> CGPath {
    let path = CGMutablePath()
    let radius = size * 0.22
    let exponent = 0.5 // x^4 + y^4 = 1, a continuous macOS-style corner.
    let steps = 32
    let centers: [(CGFloat, CGFloat, CGFloat, CGFloat)] = [
        (size - radius, radius, -.pi / 2, 0),
        (size - radius, size - radius, 0, .pi / 2),
        (radius, size - radius, .pi / 2, .pi),
        (radius, radius, .pi, 3 * .pi / 2),
    ]

    path.move(to: CGPoint(x: radius, y: 0))
    for (index, (centerX, centerY, start, end)) in centers.enumerated() {
        for step in 0...steps {
            let angle = start + (end - start) * CGFloat(step) / CGFloat(steps)
            let cosine = cos(angle)
            let sine = sin(angle)
            let x = centerX + copysign(pow(abs(cosine), exponent) * radius, cosine)
            let y = centerY + copysign(pow(abs(sine), exponent) * radius, sine)
            path.addLine(to: CGPoint(x: x, y: y))
        }
        switch index {
        case 0:
            path.addLine(to: CGPoint(x: size, y: size - radius))
        case 1:
            path.addLine(to: CGPoint(x: radius, y: size))
        case 2:
            path.addLine(to: CGPoint(x: 0, y: radius))
        default:
            path.addLine(to: CGPoint(x: radius, y: 0))
        }
    }
    path.closeSubpath()
    return path
}

func loadImage(_ url: URL) throws -> CGImage {
    guard let source = CGImageSourceCreateWithURL(url as CFURL, nil),
          let image = CGImageSourceCreateImageAtIndex(source, 0, nil)
    else {
        throw MaskIconError.cannotReadSource
    }
    return image
}

func maskedImage(_ source: CGImage) throws -> CGImage {
    let width = source.width
    let height = source.height
    guard let context = CGContext(
        data: nil,
        width: width,
        height: height,
        bitsPerComponent: 8,
        bytesPerRow: width * 4,
        space: CGColorSpaceCreateDeviceRGB(),
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
    ) else {
        throw MaskIconError.cannotCreateContext
    }
    context.interpolationQuality = .high
    context.saveGState()
    context.addPath(squirclePath(size: CGFloat(min(width, height))))
    context.clip()
    context.draw(source, in: CGRect(x: 0, y: 0, width: width, height: height))
    context.restoreGState()
    guard let image = context.makeImage() else {
        throw MaskIconError.cannotCreateImage
    }
    return image
}

func resized(_ source: CGImage, size: Int) throws -> CGImage {
    guard size > 0 else { throw MaskIconError.invalidSize }
    guard let context = CGContext(
        data: nil,
        width: size,
        height: size,
        bitsPerComponent: 8,
        bytesPerRow: size * 4,
        space: CGColorSpaceCreateDeviceRGB(),
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
    ) else {
        throw MaskIconError.cannotCreateContext
    }
    context.interpolationQuality = .high
    context.draw(source, in: CGRect(x: 0, y: 0, width: size, height: size))
    guard let image = context.makeImage() else {
        throw MaskIconError.cannotCreateImage
    }
    return image
}

func writePNG(_ image: CGImage, to url: URL) throws {
    guard let destination = CGImageDestinationCreateWithURL(
        url as CFURL,
        UTType.png.identifier as CFString,
        1,
        nil
    ) else {
        throw MaskIconError.cannotWriteOutput
    }
    CGImageDestinationAddImage(destination, image, nil)
    guard CGImageDestinationFinalize(destination) else {
        throw MaskIconError.cannotWriteOutput
    }
}

do {
    guard CommandLine.arguments.count == 4,
          let size = Int(CommandLine.arguments[3])
    else { throw MaskIconError.usage }
    let source = try loadImage(URL(fileURLWithPath: CommandLine.arguments[1]))
    let masked = try maskedImage(source)
    let output = try resized(masked, size: size)
    try writePNG(output, to: URL(fileURLWithPath: CommandLine.arguments[2]))
} catch {
    fputs("mask-icon: \(error)\n", stderr)
    exit(1)
}
