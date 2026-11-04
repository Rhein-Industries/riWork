import Foundation
import ImageIO

// Install the supplied SOMTUS artwork unchanged; this is the shared brand source.
// Usage: swift ios/scripts/create-icon.swift OUTPUT.png [SOURCE.png]
precondition(CommandLine.arguments.count >= 2, "Usage: create-icon.swift OUTPUT.png [SOURCE.png]")
let repository = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
let source = CommandLine.arguments.count > 2
    ? URL(fileURLWithPath: CommandLine.arguments[2])
    : repository.appendingPathComponent("somtus-icon-export/somtus-icon-1024.png")
let data = try Data(contentsOf: source)
guard let imageSource = CGImageSourceCreateWithData(data as CFData, nil),
      let properties = CGImageSourceCopyPropertiesAtIndex(imageSource, 0, nil) as? [CFString: Any],
      properties[kCGImagePropertyPixelWidth] as? Int == 1024,
      properties[kCGImagePropertyPixelHeight] as? Int == 1024,
      properties[kCGImagePropertyHasAlpha] as? Bool != true else {
    fatalError("SOMTUS app artwork must be an opaque 1024 × 1024 PNG.")
}
try data.write(to: URL(fileURLWithPath: CommandLine.arguments[1]), options: .atomic)
