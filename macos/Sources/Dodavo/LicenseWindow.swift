import AppKit

/// The license, and those of the components built into the program.
final class LicenseWindowController: NSWindowController {
    private struct Texts: Decodable {
        let dodavo: String
        let components: String
    }

    private let texts: Texts?
    private let choice = NSPopUpButton()
    private let textView = NSTextView()

    init() {
        texts = Core.call("licenses")
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 640, height: 520),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered, defer: false)
        window.title = L[.license_title]
        window.isReleasedWhenClosed = false
        super.init(window: window)
        build(window)
        window.center()
        showChoice()
    }

    required init?(coder _: NSCoder) { nil }

    private func build(_ window: NSWindow) {
        let label = NSTextField(labelWithString: L[.license_show_label])
        choice.addItems(withTitles: [L[.license_dodavo], L[.license_components]])
        choice.target = self
        choice.action = #selector(showChoice)
        name(choice, by: label)
        let top = NSStackView(views: [label, choice])
        top.translatesAutoresizingMaskIntoConstraints = false

        textView.isEditable = false
        textView.isSelectable = true
        textView.font = .monospacedSystemFont(ofSize: NSFont.smallSystemFontSize, weight: .regular)
        textView.setAccessibilityLabel(L[.license_title])
        textView.isVerticallyResizable = true
        textView.autoresizingMask = .width
        textView.textContainer?.widthTracksTextView = true
        let scroll = NSScrollView()
        scroll.documentView = textView
        scroll.hasVerticalScroller = true
        scroll.translatesAutoresizingMaskIntoConstraints = false

        let host = NSView()
        host.addSubview(top)
        host.addSubview(scroll)
        NSLayoutConstraint.activate([
            top.topAnchor.constraint(equalTo: host.topAnchor, constant: 12),
            top.leadingAnchor.constraint(equalTo: host.leadingAnchor, constant: 12),
            scroll.topAnchor.constraint(equalTo: top.bottomAnchor, constant: 10),
            scroll.leadingAnchor.constraint(equalTo: host.leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: host.trailingAnchor),
            scroll.bottomAnchor.constraint(equalTo: host.bottomAnchor),
        ])
        window.contentView = host
        window.initialFirstResponder = textView
        window.autorecalculatesKeyViewLoop = true
    }

    @objc private func showChoice() {
        textView.string =
            choice.indexOfSelectedItem == 0 ? texts?.dodavo ?? "" : texts?.components ?? ""
        textView.scrollToBeginningOfDocument(nil)
    }
}
