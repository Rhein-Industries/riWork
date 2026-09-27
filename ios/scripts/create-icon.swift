import Foundation
import CoreGraphics
import ImageIO
import UniformTypeIdentifiers

// Reproducible opaque app icon; no downloaded artwork or external dependency.
let context = CGContext(data: nil, width: 1024, height: 1024, bitsPerComponent: 8,
                        bytesPerRow: 4096, space: CGColorSpaceCreateDeviceRGB(),
                        bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)!
context.setFillColor(CGColor(red: 0.13, green: 0.25, blue: 0.40, alpha: 1))
context.fill(CGRect(x: 0, y: 0, width: 1024, height: 1024))
context.setStrokeColor(CGColor(red: 0.92, green: 0.96, blue: 1, alpha: 1))
context.setLineWidth(42)
context.setLineCap(.round)
context.setLineJoin(.round)
context.addPath(CGPath(roundedRect: CGRect(x: 170, y: 230, width: 684, height: 564), cornerWidth: 92, cornerHeight: 92, transform: nil))
context.strokePath()
context.move(to: CGPoint(x: 315, y: 622))
context.addLine(to: CGPoint(x: 459, y: 512))
context.addLine(to: CGPoint(x: 315, y: 402))
context.strokePath()
context.move(to: CGPoint(x: 554, y: 402))
context.addLine(to: CGPoint(x: 708, y: 402))
context.strokePath()
let url = URL(fileURLWithPath: CommandLine.arguments[1])
let destination = CGImageDestinationCreateWithURL(url as CFURL, UTType.png.identifier as CFString, 1, nil)!
CGImageDestinationAddImage(destination, context.makeImage()!, nil)
precondition(CGImageDestinationFinalize(destination))
