import AppKit

/// Says something through VoiceOver, for what changes without moving the focus.
/// Nothing happens while VoiceOver is off.
func announce(_ text: String) {
    guard !text.isEmpty else { return }
    let element: Any = NSApp.keyWindow ?? NSApp.mainWindow ?? NSApp as Any
    NSAccessibility.post(
        element: element,
        notification: .announcementRequested,
        userInfo: [
            .announcement: text,
            .priority: NSAccessibilityPriorityLevel.high.rawValue,
        ])
}

/// Text that the user reads, in a window.
func makeLabel(_ text: String = "", bold: Bool = false, small: Bool = false) -> NSTextField {
    let label = NSTextField(wrappingLabelWithString: text)
    label.isSelectable = false
    if bold {
        label.font = .boldSystemFont(ofSize: NSFont.systemFontSize)
    } else if small {
        label.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        label.textColor = .secondaryLabelColor
    }
    return label
}

func makeButton(_ title: String, _ target: AnyObject?, _ action: Selector) -> NSButton {
    NSButton(title: title, target: target, action: action)
}

/// A vertical stack that fills its width, with some room around it.
func makeColumn(_ views: [NSView], spacing: CGFloat = 8) -> NSStackView {
    let stack = NSStackView(views: views)
    stack.orientation = .vertical
    stack.alignment = .leading
    stack.spacing = spacing
    stack.translatesAutoresizingMaskIntoConstraints = false
    return stack
}

func makeRow(_ views: [NSView], spacing: CGFloat = 8) -> NSStackView {
    let stack = NSStackView(views: views)
    stack.orientation = .horizontal
    stack.alignment = .firstBaseline
    stack.spacing = spacing
    stack.translatesAutoresizingMaskIntoConstraints = false
    return stack
}

/// Puts `content` into the content view of `window`, with the margins of the
/// Human Interface Guidelines. The window's size follows from what it holds.
func fill(_ window: NSWindow, with content: NSView, margin: CGFloat = 20) {
    let host = NSView()
    host.addSubview(content)
    NSLayoutConstraint.activate([
        content.topAnchor.constraint(equalTo: host.topAnchor, constant: margin),
        content.leadingAnchor.constraint(equalTo: host.leadingAnchor, constant: margin),
        content.trailingAnchor.constraint(equalTo: host.trailingAnchor, constant: -margin),
        content.bottomAnchor.constraint(equalTo: host.bottomAnchor, constant: -margin),
    ])
    window.contentView = host
}

/// A row of buttons at the bottom of a window, on the trailing side.
func makeButtonRow(_ buttons: [NSButton]) -> NSStackView {
    let spacer = NSView()
    spacer.setContentHuggingPriority(.defaultLow, for: .horizontal)
    let row = NSStackView(views: [spacer] + buttons)
    row.orientation = .horizontal
    row.spacing = 12
    row.translatesAutoresizingMaskIntoConstraints = false
    return row
}

/// An alert as a sheet on `window`, or on its own without one.
func showAlert(_ text: String, informative: String = "", on window: NSWindow?) {
    let alert = NSAlert()
    alert.messageText = text
    alert.informativeText = informative
    alert.addButton(withTitle: L[.close])
    if let window = window, window.isVisible {
        alert.beginSheetModal(for: window, completionHandler: nil)
    } else {
        NSApp.activate(ignoringOtherApps: true)
        alert.runModal()
    }
}
