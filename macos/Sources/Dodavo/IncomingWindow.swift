import AppKit

/// Someone wants to send something. It comes to the front without taking the
/// focus from what the user is doing; a notification and the Dock say so.
final class IncomingWindowController: NSWindowController, NSWindowDelegate {
    private let offer: OfferInfo
    private let closed: () -> Void
    private var dir: String
    private var answered = false
    private let dirLabel = NSTextField(wrappingLabelWithString: "")
    private let spaceLine = makeLabel()
    private let addCheck = NSButton(checkboxWithTitle: "", target: nil, action: nil)

    init(offer: OfferInfo, closed: @escaping () -> Void) {
        self.offer = offer
        self.closed = closed
        dir = offer.dir
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 480, height: 220),
            styleMask: [.titled, .closable, .miniaturizable],
            backing: .buffered, defer: false)
        window.title = L[.incoming_title]
        window.isReleasedWhenClosed = false
        super.init(window: window)
        window.delegate = self
        build(window)
        window.center()
    }

    required init?(coder _: NSCoder) { nil }

    private func build(_ window: NSWindow) {
        let width: CGFloat = 440
        func sized(_ label: NSTextField) -> NSTextField {
            label.preferredMaxLayoutWidth = width
            return label
        }
        var views: [NSView] = [
            sized(makeLabel(offer.headline, bold: true)),
            sized(makeLabel(offer.description)),
        ]
        if let warning = offer.unknownWarning, let label = offer.addLabel {
            views.append(sized(makeLabel(warning)))
            addCheck.title = label
            views.append(addCheck)
        }
        let saveLabel = NSTextField(labelWithString: L[.save_to])
        dirLabel.isSelectable = false
        dirLabel.stringValue = dir
        dirLabel.preferredMaxLayoutWidth = width - 190
        let choose = makeButton(L[.choose], self, #selector(chooseFolder))
        choose.setAccessibilityTitle("\(saveLabel.stringValue) \(L[.choose])")
        name(dirLabel, by: saveLabel)
        views.append(makeRow([saveLabel, dirLabel, choose]))
        spaceLine.stringValue = offer.space.text
        views.append(sized(spaceLine))
        let decline = makeButton(L[.decline], self, #selector(declineOffer))
        decline.keyEquivalent = "\u{1b}"
        let accept = makeButton(L[.accept], self, #selector(acceptOffer))
        accept.keyEquivalent = "\r"
        let buttons = makeButtonRow([decline, accept])
        views.append(buttons)
        let column = makeColumn(views, spacing: 10)
        fill(window, with: column)
        buttons.widthAnchor.constraint(equalTo: column.widthAnchor).isActive = true
        window.initialFirstResponder = accept
        window.autorecalculatesKeyViewLoop = true
    }

    /// In front of the other windows, leaving the focus where it is.
    func present() {
        window?.orderFrontRegardless()
    }

    func bringToFront() {
        window?.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    private func refreshSpace() {
        let space: SpaceInfo? = Core.call("space", ["id": offer.id, "dir": dir])
        if let space = space {
            spaceLine.stringValue = space.text
            // After VoiceOver has told where the focus went with the panel gone.
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) { announce(space.text) }
        }
    }

    @objc private func chooseFolder() {
        guard let window = window else { return }
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.canCreateDirectories = true
        panel.prompt = L[.choose_folder_prompt]
        panel.directoryURL = URL(fileURLWithPath: dir)
        panel.beginSheetModal(for: window) { response in
            guard response == .OK, let url = panel.url else { return }
            self.dir = url.path
            self.dirLabel.stringValue = url.path
            self.refreshSpace()
        }
    }

    private struct Check: Decodable {
        let problem: String?
        let confirm: String?
    }

    @objc private func acceptOffer() {
        guard let window = window else { return }
        let check: Check? = Core.call("check_accept", ["id": offer.id, "dir": dir])
        if let problem = check?.problem {
            showAlert(problem, on: window)
            return
        }
        guard let question = check?.confirm else {
            finishAccepting()
            return
        }
        let alert = NSAlert()
        alert.messageText = question
        alert.addButton(withTitle: L[.accept_anyway])
        alert.addButton(withTitle: L[.cancel])
        alert.buttons[1].keyEquivalent = "\u{1b}"
        alert.beginSheetModal(for: window) { response in
            if response == .alertFirstButtonReturn { self.finishAccepting() }
        }
    }

    private func finishAccepting() {
        if addCheck.state == .on, offer.addLabel != nil {
            // A name that is taken is not worth losing the transfer for: the
            // sender stays unknown, and can be added afterwards.
            _ = Core.problemOf(
                "check_recipient", ["name": offer.fromName, "contact_id": offer.fromId])
                ?? Core.errorOf(
                    "add_recipient", ["name": offer.fromName, "contact_id": offer.fromId])
            (NSApp.delegate as? AppDelegate)?.recipientsChanged()
        }
        answered = true
        Core.perform("accept", ["id": offer.id, "dir": dir])
        close()
    }

    @objc private func declineOffer() {
        window?.performClose(nil)
    }

    func closeWithoutAnswer() {
        answered = true
        close()
    }

    func windowWillClose(_: Notification) {
        if !answered {
            answered = true
            Core.perform("decline", ["id": offer.id])
        }
        closed()
    }
}
