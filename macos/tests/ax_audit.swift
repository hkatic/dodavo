// Walks the windows of the running Dodavo as VoiceOver sees them, and fails on a
// control without a name. It needs the screen unlocked, and the Accessibility
// permission for the program that runs it (Privacy & Security): macOS reports
// neither a window nor an error otherwise.
//
//   xcrun swiftc -O macos/tests/ax_audit.swift -o target/ax_audit && target/ax_audit [--tree]
import AppKit
import ApplicationServices

func attribute(_ element: AXUIElement, _ name: String) -> AnyObject? {
    var value: AnyObject?
    let status = AXUIElementCopyAttributeValue(element, name as CFString, &value)
    return status == .success ? value : nil
}

func text(_ element: AXUIElement, _ name: String) -> String {
    guard let value = attribute(element, name) else { return "" }
    if let string = value as? String { return string.replacingOccurrences(of: "\n", with: " ") }
    if let number = value as? NSNumber { return number.stringValue }
    return ""
}

let printTree = CommandLine.arguments.contains("--tree")
var problems = 0
var controls = 0

/// Roles that a user acts on, and so have to say what they are.
let named: Set<String> = [
    "AXButton", "AXCheckBox", "AXRadioButton", "AXPopUpButton", "AXMenuButton", "AXTextField",
    "AXTextArea", "AXSlider", "AXProgressIndicator", "AXTable", "AXOutline", "AXLink",
]

func walk(_ element: AXUIElement, depth: Int, path: String) {
    let role = text(element, "AXRole")
    let parts = ["AXTitle", "AXDescription"].map { text(element, $0) }.filter { !$0.isEmpty }
    let hasLabel = attribute(element, "AXTitleUIElement") != nil
    let here = "\(path)/\(role)"
    if printTree {
        let line = ([role] + parts + (hasLabel ? ["(labelled)"] : [])).joined(separator: " | ")
        print(String(repeating: "  ", count: depth) + line)
    }
    if named.contains(role) {
        controls += 1
        if parts.isEmpty && !hasLabel {
            problems += 1
            print("NO NAME: \(here) value=\(text(element, "AXValue").prefix(40))")
        }
    }
    guard depth < 14, let children = attribute(element, "AXChildren") as? [AXUIElement] else {
        return
    }
    for child in children { walk(child, depth: depth + 1, path: here) }
}

guard AXIsProcessTrusted() else {
    print("This program lacks the Accessibility permission (Privacy & Security).")
    exit(2)
}
guard
    let app = NSRunningApplication.runningApplications(
        withBundleIdentifier: "io.github.hkatic.dodavo"
    ).first
else {
    print("Dodavo is not running.")
    exit(2)
}
let windows =
    attribute(AXUIElementCreateApplication(app.processIdentifier), "AXWindows") as? [AXUIElement]
    ?? []
guard !windows.isEmpty, text(windows[0], "AXRole") != "AXApplication" else {
    print("No windows to look at: is the screen locked?")
    exit(2)
}
for window in windows {
    let title = text(window, "AXTitle")
    print("Window: \(title)")
    walk(window, depth: 0, path: title)
}
print("\(controls) controls, \(problems) without a name")
exit(problems == 0 ? 0 : 1)
