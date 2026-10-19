// One WtAgent-style JSON request on stdin, one JSON response on stdout.
// No listener, shell evaluation, clipboard writes or request argument logging.
import AppKit
import ApplicationServices
import Foundation
import ScreenCaptureKit

struct AgentFailure: Error {
    let code: String
    let message: String
    init(_ code: String, _ message: String) {
        self.code = code
        self.message = message
    }
}

typealias Args = [String: Any]

func string(_ args: Args, _ key: String, _ fallback: String = "") -> String {
    args[key] as? String ?? fallback
}

func integer(_ args: Args, _ key: String, _ fallback: Int = 0) -> Int {
    (args[key] as? NSNumber)?.intValue ?? fallback
}

func attribute(_ element: AXUIElement, _ name: String) -> CFTypeRef? {
    var result: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, name as CFString, &result) == .success else {
        return nil
    }
    return result
}

func axString(_ element: AXUIElement, _ name: String) -> String {
    attribute(element, name) as? String ?? ""
}

func rect(_ element: AXUIElement) -> [Double] {
    var point = CGPoint.zero
    var size = CGSize.zero
    if let value = attribute(element, kAXPositionAttribute), CFGetTypeID(value) == AXValueGetTypeID() {
        AXValueGetValue(value as! AXValue, .cgPoint, &point)
    }
    if let value = attribute(element, kAXSizeAttribute), CFGetTypeID(value) == AXValueGetTypeID() {
        AXValueGetValue(value as! AXValue, .cgSize, &size)
    }
    return [point.x, point.y, size.width, size.height]
}

func describe(_ element: AXUIElement, depth: Int = 0) -> Args {
    let role = axString(element, kAXRoleAttribute)
    let title = axString(element, kAXTitleAttribute)
    let description = axString(element, kAXDescriptionAttribute)
    let value = attribute(element, kAXValueAttribute)
    let secure = axString(element, kAXSubroleAttribute) == kAXSecureTextFieldSubrole
    var result: Args = ["role": role, "type": role, "depth": depth, "rect": rect(element),
                        "name": !title.isEmpty ? title : description,
                        "enabled": attribute(element, kAXEnabledAttribute) as? Bool ?? true]
    if let selected = attribute(element, kAXSelectedAttribute) { result["selected"] = selected }
    if secure {
        result["value"] = "<redacted>"
    } else if let value = value as? String {
        result["value"] = value
        if string(result, "name").isEmpty && role == kAXStaticTextRole { result["name"] = value }
    } else if let value = value as? NSNumber {
        result["value"] = value
    }
    return result
}

func matchesRegex(_ text: String, _ pattern: String) throws -> Bool {
    let regex: NSRegularExpression
    do { regex = try NSRegularExpression(pattern: pattern, options: [.caseInsensitive]) }
    catch { throw AgentFailure("BAD_SELECTOR", "Invalid regular expression") }
    return regex.firstMatch(in: text, range: NSRange(text.startIndex..., in: text)) != nil
}

let roleAliases: [String: [String]] = [
    "Button": [kAXButtonRole], "Edit": [kAXTextFieldRole, kAXTextAreaRole],
    "Text": [kAXStaticTextRole], "CheckBox": [kAXCheckBoxRole],
    "RadioButton": [kAXRadioButtonRole], "TabItem": [kAXRadioButtonRole, "AXTab", kAXButtonRole],
    "ComboBox": [kAXComboBoxRole, kAXPopUpButtonRole], "Window": [kAXWindowRole],
]

func matches(_ info: Args, _ args: Args) throws -> Bool {
    let name = string(info, "name")
    if let exact = args["name"] as? String, exact != name { return false }
    if let label = args["label"] as? String, label != name { return false }
    if let regex = args["name_re"] as? String, try !matchesRegex(name, regex) { return false }
    let role = string(args, "role", string(args, "type"))
    if !role.isEmpty && !(roleAliases[role] ?? [role]).contains(string(info, "role")) { return false }
    if let enabled = args["enabled"] as? Bool, enabled != info["enabled"] as? Bool { return false }
    return true
}

func applications(_ args: Args) throws -> [NSRunningApplication] {
    try NSWorkspace.shared.runningApplications.filter { app in
        if let pid = args["pid"] as? Int, app.processIdentifier != pid { return false }
        let executable = app.executableURL?.lastPathComponent ?? app.localizedName ?? ""
        return try matchesRegex(executable, string(args, "process_re", "^superba$"))
    }
}

func windows(_ args: Args) throws -> [(NSRunningApplication, AXUIElement)] {
    var result: [(NSRunningApplication, AXUIElement)] = []
    for app in try applications(args) {
        let root = AXUIElementCreateApplication(app.processIdentifier)
        AXUIElementSetMessagingTimeout(root, 2)
        // AccessKit activates its lazy tree when an accessibility client asks.
        AXUIElementSetAttributeValue(root, "AXManualAccessibility" as CFString, kCFBooleanTrue)
        let found = attribute(root, kAXWindowsAttribute) as? [AXUIElement] ?? []
        for window in found {
            if let title = args["title_re"] as? String,
               try !matchesRegex(axString(window, kAXTitleAttribute), title) { continue }
            result.append((app, window))
        }
    }
    return result
}

func nodes(_ args: Args) throws -> [(AXUIElement, Args)] {
    var result: [(AXUIElement, Args)] = []
    var seen: [CFHashCode: [AXUIElement]] = [:]
    let limit = min(max(integer(args, "max_nodes", 6000), 1), 20000)
    var queue = try windows(args).map { ($0.1, 0) }
    var cursor = 0
    while cursor < queue.count {
        let (element, depth) = queue[cursor]
        cursor += 1
        let hash = CFHash(element)
        if seen[hash, default: []].contains(where: { CFEqual($0, element) }) { continue }
        seen[hash, default: []].append(element)
        guard result.count < limit && depth < 100 else {
            throw AgentFailure("TREE_LIMIT", "Accessibility tree exceeded traversal limit")
        }
        result.append((element, describe(element, depth: depth)))
        let children = attribute(element, kAXChildrenAttribute) as? [AXUIElement] ?? []
        queue.append(contentsOf: children.map { ($0, depth + 1) })
    }
    return result
}

func find(_ args: Args) throws -> [(AXUIElement, Args)] {
    let end = Date().addingTimeInterval(Double(min(integer(args, "timeout_ms"), 120000)) / 1000)
    let index = integer(args, "index")
    guard index >= 0 else { throw AgentFailure("BAD_SELECTOR", "index must be nonnegative") }
    while true {
        let result = try nodes(args).filter { try matches($0.1, args) }
        if result.count > index || Date() >= end { return result }
        Thread.sleep(forTimeInterval: 0.2)
    }
}

func checked(_ error: AXError) throws {
    guard error == .success else {
        throw AgentFailure("AX_ERROR", "Accessibility operation failed (\(error.rawValue))")
    }
}

func key(_ code: CGKeyCode, flags: CGEventFlags = []) throws {
    guard let down = CGEvent(keyboardEventSource: nil, virtualKey: code, keyDown: true),
          let up = CGEvent(keyboardEventSource: nil, virtualKey: code, keyDown: false) else {
        throw AgentFailure("INPUT_ERROR", "Could not create keyboard event")
    }
    down.flags = flags
    up.flags = flags
    down.post(tap: .cghidEventTap)
    up.post(tap: .cghidEventTap)
}

func typeText(_ text: String) throws {
    for character in text {
        let units = Array(String(character).utf16)
        guard let down = CGEvent(keyboardEventSource: nil, virtualKey: 0, keyDown: true),
              let up = CGEvent(keyboardEventSource: nil, virtualKey: 0, keyDown: false) else {
            throw AgentFailure("INPUT_ERROR", "Could not create text event")
        }
        units.withUnsafeBufferPointer { buffer in
            down.keyboardSetUnicodeString(stringLength: buffer.count, unicodeString: buffer.baseAddress)
            up.keyboardSetUnicodeString(stringLength: buffer.count, unicodeString: buffer.baseAddress)
        }
        down.post(tap: .cghidEventTap)
        up.post(tap: .cghidEventTap)
    }
}

func foreground(_ args: Args) throws -> AXUIElement {
    guard let (app, window) = try windows(args).first else {
        throw AgentFailure("NO_WINDOW", "No matching window")
    }
    app.activate(options: [.activateAllWindows])
    _ = AXUIElementPerformAction(window, kAXRaiseAction as CFString)
    return window
}

func act(_ args: Args) throws -> Any {
    let index = integer(args, "index")
    let found = try find(args)
    guard found.indices.contains(index) else { throw AgentFailure("NOT_FOUND", "No matching element") }
    let element = found[index].0
    switch string(args, "action", "invoke") {
    case "invoke", "click", "toggle", "select":
        try checked(AXUIElementPerformAction(element, kAXPressAction as CFString))
    case "set_value":
        try checked(AXUIElementSetAttributeValue(element, kAXValueAttribute as CFString,
                                               string(args, "value") as CFString))
    case "type":
        _ = try foreground(args)
        try checked(AXUIElementSetAttributeValue(element, kAXFocusedAttribute as CFString, kCFBooleanTrue))
        try key(0, flags: .maskCommand)
        try typeText(string(args, "value"))
    case "value": return describe(element)["value"] ?? NSNull()
    default: throw AgentFailure("BAD_ACTION", "Unsupported action")
    }
    Thread.sleep(forTimeInterval: Double(min(integer(args, "settle_ms", 200), 5000)) / 1000)
    return true
}

func permissions(_ prompt: Bool = false) -> Args {
    let trusted = AXIsProcessTrustedWithOptions(
        [kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: prompt] as CFDictionary)
    return ["accessibility": trusted, "screen_capture": CGPreflightScreenCaptureAccess(),
            "executable": URL(fileURLWithPath: CommandLine.arguments[0]).standardizedFileURL.path,
            "instructions": "System Settings > Privacy & Security > Accessibility: add and enable this executable. " +
                "For screenshots also allow it in Screen & System Audio Recording. Restart the command after allowing it."]
}

func screenshot(_ args: Args) async throws -> Any {
    guard CGPreflightScreenCaptureAccess() else {
        throw AgentFailure("TCC_SCREEN_CAPTURE", "Screen recording permission is missing; see permissions command")
    }
    let content = try await SCShareableContent.excludingDesktopWindows(true, onScreenWindowsOnly: true)
    let pids = Set(try applications(args).map { $0.processIdentifier })
    let candidates = try content.windows.filter { window in
        guard let owner = window.owningApplication, pids.contains(owner.processID),
              window.frame.width > 100, window.frame.height > 100 else { return false }
        if let title = args["title_re"] as? String {
            return try matchesRegex(window.title ?? "", title)
        }
        return true
    }
    let index = integer(args, "index")
    guard candidates.indices.contains(index) else { throw AgentFailure("NO_WINDOW", "No capturable window") }
    let window = candidates[index]
    let filter = SCContentFilter(desktopIndependentWindow: window)
    let config = SCStreamConfiguration()
    config.width = Int(filter.contentRect.width * CGFloat(filter.pointPixelScale))
    config.height = Int(filter.contentRect.height * CGFloat(filter.pointPixelScale))
    config.showsCursor = false
    let cgImage = try await SCScreenshotManager.captureImage(contentFilter: filter, configuration: config)
    let bitmap = NSBitmapImageRep(cgImage: cgImage)
    guard let data = bitmap.representation(using: .png, properties: [:]), !string(args, "path").isEmpty else {
        throw AgentFailure("SCREENSHOT_ERROR", "PNG encoding or destination missing")
    }
    try data.write(to: URL(fileURLWithPath: string(args, "path")), options: .atomic)
    return ["path": string(args, "path"), "window_id": window.windowID,
            "width": cgImage.width, "height": cgImage.height]
}

func dispatch(_ command: String, _ args: Args) async throws -> Any {
    if command == "ping" || command == "permissions" {
        return permissions(args["prompt"] as? Bool ?? false)
    }
    guard AXIsProcessTrusted() else {
        throw AgentFailure("TCC_ACCESSIBILITY", "Accessibility permission is missing; see permissions command")
    }
    switch command {
    case "windows":
        return try windows(args).map { app, window -> Args in
            ["pid": app.processIdentifier, "process": app.executableURL?.lastPathComponent ?? "",
             "title": axString(window, kAXTitleAttribute), "rect": rect(window)]
        }
    case "window":
        let window = try foreground(args)
        if args["w"] != nil && args["h"] != nil {
            var size = CGSize(width: integer(args, "w"), height: integer(args, "h"))
            try checked(AXUIElementSetAttributeValue(window, kAXSizeAttribute as CFString,
                                                     AXValueCreate(.cgSize, &size)!))
        }
        return describe(window)
    case "uia.tree", "ax.tree": return try nodes(args).map { $0.1 }
    case "uia.find", "ax.find", "wait": return try find(args).map { $0.1 }
    case "uia.act", "ax.act": return try act(args)
    case "key":
        _ = try foreground(args)
        let keys: [String: CGKeyCode] = ["A": 0, "RETURN": 36, "ENTER": 36, "TAB": 48,
                                       "SPACE": 49, "ESC": 53, "ESCAPE": 53,
                                       "LEFT": 123, "RIGHT": 124, "DOWN": 125, "UP": 126]
        let parts = string(args, "combo").uppercased().split(separator: "+").map(String.init)
        guard let code = keys[parts.last ?? ""] else { throw AgentFailure("BAD_KEY", "Unsupported key") }
        var flags: CGEventFlags = []
        if parts.contains("CMD") || parts.contains("COMMAND") { flags.insert(.maskCommand) }
        if parts.contains("CTRL") { flags.insert(.maskControl) }
        if parts.contains("SHIFT") { flags.insert(.maskShift) }
        if parts.contains("ALT") { flags.insert(.maskAlternate) }
        for _ in 0..<min(max(integer(args, "repeat", 1), 1), 100) { try key(code, flags: flags) }
        return true
    case "shot": return try await screenshot(args)
    default: throw AgentFailure("BAD_COMMAND", "Unsupported command")
    }
}

@main
struct MacAgent {
    static func main() async {
        let start = Date()
        var response: Args
        do {
            guard let line = readLine(), let data = line.data(using: .utf8),
                  let request = try JSONSerialization.jsonObject(with: data) as? Args else {
                throw AgentFailure("BAD_REQUEST", "Expected one JSON request on stdin")
            }
            let result = try await dispatch(string(request, "cmd"), request["args"] as? Args ?? [:])
            response = ["ok": true, "result": result]
        } catch let failure as AgentFailure {
            response = ["ok": false, "code": failure.code, "error": failure.message]
        } catch {
            // System error descriptions can contain paths or UI values. Keep the protocol generic.
            response = ["ok": false, "code": "SYSTEM_ERROR", "error": "System operation failed"]
        }
        response["ms"] = Int(Date().timeIntervalSince(start) * 1000)
        let data = try! JSONSerialization.data(withJSONObject: response, options: [.sortedKeys])
        FileHandle.standardOutput.write(data)
        FileHandle.standardOutput.write(Data([10]))
        if response["ok"] as? Bool != true { exit(1) }
    }
}
