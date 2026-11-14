import Foundation
import CoreGraphics
import ImageIO
import Darwin

// Output contains only the requested child's window geometry, never the other
// applications' names, titles, or window list. No permission prompts are issued.
enum ProbeError: Error {
    case failed(String)
}

func emit(_ result: [String: Any]) throws {
    let data = try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys])
    FileHandle.standardOutput.write(data)
    FileHandle.standardOutput.write(Data("\n".utf8))
}

func findWindow(pid: Int32) throws {
    guard let windows = CGWindowListCopyWindowInfo(
        [.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID
    ) as? [[String: Any]] else {
        throw ProbeError.failed("WindowServer did not return a window list")
    }
    var candidates: [(id: UInt32, bounds: CGRect)] = []
    for window in windows {
        guard (window[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value == pid,
              (window[kCGWindowLayer as String] as? NSNumber)?.intValue == 0,
              (window[kCGWindowIsOnscreen as String] as? NSNumber)?.boolValue == true,
              let id = (window[kCGWindowNumber as String] as? NSNumber)?.uint32Value,
              id != kCGNullWindowID,
              let dictionary = window[kCGWindowBounds as String] as? [String: Any],
              let bounds = CGRect(dictionaryRepresentation: dictionary as CFDictionary),
              bounds.width >= 100, bounds.height >= 100,
              ((window[kCGWindowAlpha as String] as? NSNumber)?.doubleValue ?? 1) > 0
        else { continue }
        candidates.append((id, bounds))
    }
    guard let selected = candidates.max(by: {
        $0.bounds.width * $0.bounds.height < $1.bounds.width * $1.bounds.height
    }) else {
        // Exit 1 means "not ready"; Python retries while the child is alive.
        exit(1)
    }
    try emit([
        "pid": Int(pid), "window_id": selected.id,
        "x": Double(selected.bounds.origin.x), "y": Double(selected.bounds.origin.y),
        "width": Double(selected.bounds.width), "height": Double(selected.bounds.height)
    ])
}

func checkImage(path: String) throws {
    let url = URL(fileURLWithPath: path)
    guard let source = CGImageSourceCreateWithURL(url as CFURL, nil),
          let image = CGImageSourceCreateImageAtIndex(source, 0, nil) else {
        throw ProbeError.failed("ImageIO could not decode the window screenshot")
    }
    let width = image.width
    let height = image.height
    guard width >= 100, height >= 100, width <= 8192, height <= 8192,
          width * height <= 16_777_216 else {
        throw ProbeError.failed("Window screenshot dimensions are invalid or excessive")
    }
    var pixels = [UInt8](repeating: 0, count: width * height * 4)
    let colorCount = try pixels.withUnsafeMutableBytes { bytes -> Int in
        let info = CGImageAlphaInfo.premultipliedLast.rawValue
            | CGBitmapInfo.byteOrder32Big.rawValue
        guard let context = CGContext(
            data: bytes.baseAddress, width: width, height: height,
            bitsPerComponent: 8, bytesPerRow: width * 4,
            space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: info
        ) else {
            throw ProbeError.failed("Could not allocate screenshot decoding context")
        }
        context.draw(image, in: CGRect(x: 0, y: 0, width: CGFloat(width), height: CGFloat(height)))
        let rgba = bytes.bindMemory(to: UInt8.self)
        var colors = Set<UInt32>()
        // Stop at 17: the reported value is a lower bound, not an exact count.
        for offset in stride(from: 0, to: rgba.count, by: 4) where rgba[offset + 3] != 0 {
            let rgb = UInt32(rgba[offset]) << 16
                | UInt32(rgba[offset + 1]) << 8 | UInt32(rgba[offset + 2])
            colors.insert(rgb)
            if colors.count > 16 { break }
        }
        return colors.count
    }
    guard colorCount > 16 else {
        throw ProbeError.failed("Window screenshot has at most 16 visible RGB colors")
    }
    try emit([
        "width": width, "height": height,
        "unique_visible_rgb_colors_at_least": colorCount
    ])
}

do {
    let arguments = Array(CommandLine.arguments.dropFirst())
    guard arguments.count == 2 else {
        throw ProbeError.failed("Usage: macos-window --pid PID | --check-image FILE")
    }
    switch arguments[0] {
    case "--pid":
        guard let pid = Int32(arguments[1]), pid > 0 else {
            throw ProbeError.failed("Expected a positive child PID")
        }
        try findWindow(pid: pid)
    case "--check-image":
        try checkImage(path: arguments[1])
    default:
        throw ProbeError.failed("Unknown window probe operation")
    }
} catch {
    let message: String
    if case ProbeError.failed(let detail) = error {
        message = detail
    } else {
        message = "Window probe failed"
    }
    FileHandle.standardError.write(Data((message + "\n").utf8))
    exit(2)
}
