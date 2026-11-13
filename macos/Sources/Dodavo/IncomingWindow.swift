import AppKit

/// Someone wants to send something. It comes to the front without taking the
/// focus from what the user is doing; a notification and the Dock say so.
///
/// All the offers of one sender that wait for an answer share a window, with
/// one answer. Several selected items arrive as that many offers, and windows
/// of their own would open exactly on top of each other: answering the one in
/// front would leave the others behind it, waiting unseen.
final class IncomingWindowController: NSWindowController, NSWindowDelegate, NSTableViewDataSource,
    NSTableViewDelegate
{
    /// An offer that waits for the answer.
    private struct Waiting {
        let id: UInt64
        let since: Date
    }

    /// An item that arrived more recently than this is not covered by Accept:
    /// nobody can have seen it, and Return may be on its way already. It stays
    /// in the window.
    private static let seenAfter: TimeInterval = 1
    /// Offers of a selection arrive one by one; what the window says of them
    /// is asked for, and said, once they have stopped coming.
    private static let settleAfter: TimeInterval = 0.3

    /// What the window opened with: who sends, and what to say of them.
    private let offer: OfferInfo
    private var waiting: [Waiting]
    /// The offer `id` waits in this window no more.
    private let left: (UInt64) -> Void
    /// Tells of an offer that was added while the window was not in front.
    private let tell: (Notice, UInt64) -> Void
    private var dir: String
    private var answered = false
    /// Whether the sender is a recipient by now.
    private var known: Bool
    /// An offer was added since the window last said what waits, with what
    /// tells of it elsewhere.
    private var added: (notice: Notice, id: UInt64)?
    private var settle: DispatchWorkItem?
    /// Each item, when there are several.
    private var items: [String] = []

    private let headline = makeLabel(bold: true)
    private let descriptionLine = makeLabel()
    private let limitLine = makeLabel()
    private let itemsLabel = NSTextField(labelWithString: L[.incoming_items_label])
    private let itemsTable = NSTableView()
    private let itemsScroll = NSScrollView()
    private let warningLine = makeLabel()
    private let dirLabel = NSTextField(wrappingLabelWithString: "")
    private let spaceLine = makeLabel()
    private let addCheck = NSButton(checkboxWithTitle: "", target: nil, action: nil)

    init(
        offer: OfferInfo, left: @escaping (UInt64) -> Void,
        tell: @escaping (Notice, UInt64) -> Void
    ) {
        self.offer = offer
        self.left = left
        self.tell = tell
        waiting = [Waiting(id: offer.id, since: Date())]
        dir = offer.dir
        known = offer.known
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
        headline.stringValue = offer.headline
        descriptionLine.stringValue = offer.description
        limitLine.stringValue = offer.overLimit ?? ""
        limitLine.isHidden = offer.overLimit == nil
        var views: [NSView] = [sized(headline), sized(descriptionLine), sized(limitLine)]

        // A label and a list that only appear when there are several items.
        let column = NSTableColumn(identifier: .init("item"))
        column.resizingMask = .autoresizingMask
        itemsTable.addTableColumn(column)
        itemsTable.headerView = nil
        itemsTable.columnAutoresizingStyle = .uniformColumnAutoresizingStyle
        itemsTable.dataSource = self
        itemsTable.delegate = self
        name(itemsTable, by: itemsLabel)
        itemsScroll.documentView = itemsTable
        itemsScroll.hasVerticalScroller = true
        itemsScroll.borderType = .bezelBorder
        itemsScroll.translatesAutoresizingMaskIntoConstraints = false
        itemsScroll.heightAnchor.constraint(equalToConstant: 96).isActive = true
        itemsLabel.isHidden = true
        itemsScroll.isHidden = true
        views.append(itemsLabel)
        views.append(itemsScroll)

        if let warning = offer.unknownWarning, let label = offer.addLabel {
            warningLine.stringValue = warning
            views.append(sized(warningLine))
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
        // On the leading side, apart from the answers.
        let block = makeButton(L[.block_sender], self, #selector(blockSender))
        // Command-B, as Block Recipient has in the main window.
        block.keyEquivalent = "b"
        block.keyEquivalentModifierMask = [.command]
        // To ask the sender what this is, before answering.
        let chat = makeButton(L[.menu_chat], self, #selector(chat(_:)))
        let buttons = makeButtonRow([decline, accept], leading: [block, chat])
        views.append(buttons)
        let stack = makeColumn(views, spacing: 10)
        fill(window, with: stack)
        buttons.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        itemsScroll.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
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

    // MARK: The offers that wait here

    private var ids: [UInt64] {
        waiting.map { $0.id }
    }

    /// Whether `other` goes into this window: an offer of the same sender,
    /// while the window still waits for its answer and asks nothing (a window
    /// that has a sheet open would hold the new offer up).
    func takes(_ other: OfferInfo) -> Bool {
        guard let window = window, window.isVisible, window.attachedSheet == nil else {
            return false
        }
        return !answered && other.fromId.caseInsensitiveCompare(offer.fromId) == .orderedSame
    }

    /// Another offer of the same sender waits here too. `notice` tells of it
    /// while the window is not in front.
    func add(_ other: OfferInfo, notice: Notice) {
        waiting.append(Waiting(id: other.id, since: Date()))
        added = (notice, other.id)
        changed()
    }

    /// The offer `id` waits no more: its sender gave up, or it was declined
    /// from elsewhere. The window closes when that was the last one.
    func remove(_ id: UInt64) {
        guard waiting.contains(where: { $0.id == id }) else { return }
        waiting.removeAll { $0.id == id }
        if waiting.isEmpty {
            answered = true
            close()
        } else {
            changed()
        }
    }

    /// What waits here has changed: the window says so once the changes stop.
    private func changed() {
        settle?.cancel()
        let work = DispatchWorkItem { [weak self] in self?.settled() }
        settle = work
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.settleAfter, execute: work)
    }

    private func settled() {
        guard !answered, !waiting.isEmpty else { return }
        show()
        guard let (notice, id) = added else { return }
        added = nil
        // Where the user is looking, VoiceOver says it; elsewhere the
        // notification does, and never both.
        if NSApp.isActive, window?.isKeyWindow == true {
            announce(headline.stringValue, urgent: false)
        } else {
            NSApp.requestUserAttention(.informationalRequest)
            tell(notice, id)
        }
    }

    /// Says what waits for an answer, and arranges the window for it.
    private func show() {
        let info: OffersInfo? = Core.call("offers", ["ids": ids, "dir": dir])
        guard let info = info else { return }
        headline.stringValue = info.headline
        descriptionLine.stringValue = info.description
        limitLine.stringValue = info.overLimit ?? ""
        limitLine.isHidden = info.overLimit == nil
        spaceLine.stringValue = info.space.text
        items = info.items
        itemsTable.reloadData()
        // The focus must not stay on a list that goes away.
        if items.isEmpty, window?.firstResponder === itemsTable {
            window?.makeFirstResponder(window?.initialFirstResponder)
        }
        itemsLabel.isHidden = items.isEmpty
        itemsScroll.isHidden = items.isEmpty
        warningLine.isHidden = known
        addCheck.isHidden = known
        fit()
    }

    /// Gives the window the height of what it holds now, with its title bar
    /// where it was and no narrower than it is.
    private func fit() {
        guard let window = window, let content = window.contentView else { return }
        content.layoutSubtreeIfNeeded()
        let fitting = content.fittingSize
        let top = window.frame.maxY
        window.setContentSize(
            NSSize(width: max(fitting.width, content.frame.width), height: fitting.height))
        window.setFrameTopLeftPoint(NSPoint(x: window.frame.minX, y: top))
    }

    func numberOfRows(in _: NSTableView) -> Int {
        items.count
    }

    func tableView(_ tableView: NSTableView, viewFor _: NSTableColumn?, row: Int) -> NSView? {
        guard items.indices.contains(row) else { return nil }
        let id = NSUserInterfaceItemIdentifier("ItemCell")
        let cell = tableView.makeView(withIdentifier: id, owner: self) as? NSTableCellView ?? {
            let cell = NSTableCellView()
            cell.identifier = id
            let text = NSTextField(labelWithString: "")
            text.lineBreakMode = .byTruncatingMiddle
            text.translatesAutoresizingMaskIntoConstraints = false
            cell.addSubview(text)
            cell.textField = text
            NSLayoutConstraint.activate([
                text.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 4),
                text.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -4),
                text.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
            ])
            return cell
        }()
        cell.textField?.stringValue = items[row]
        // All of it is heard, also where the row cuts it short.
        cell.textField?.toolTip = items[row]
        return cell
    }

    // MARK: What the user does

    private func refreshSpace() {
        let space: SpaceInfo? = Core.call("space", ["ids": ids, "dir": dir])
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
        let tempWarning: String?
        let confirm: String?
    }

    /// What an Accept at `now` covers. The first item is what the window
    /// opened with, so it goes as a single offer always did.
    private func seen(at now: Date) -> [UInt64] {
        waiting.enumerated().compactMap { index, item in
            index == 0 || now.timeIntervalSince(item.since) >= Self.seenAfter ? item.id : nil
        }
    }

    @objc private func acceptOffer() {
        accept(seen(at: Date()), ignoringTemp: false)
    }

    /// Accepts the offers `accepted` unless something stands in the way.
    /// `ignoringTemp`: the user has heard of the temporary file that is too
    /// large for its volume, and goes on anyway.
    private func accept(_ accepted: [UInt64], ignoringTemp: Bool) {
        guard let window = window else { return }
        let check: Check? = Core.call(
            "check_accept", ["ids": accepted, "dir": dir, "ignore_temp": ignoringTemp])
        if let problem = check?.problem {
            showAlert(problem, on: window)
            return
        }
        if let warning = check?.tempWarning {
            warnAboutTempFolder(
                warning, on: window,
                again: { [weak self] in
                    // With another folder for temporary files, the line
                    // about the space may say something else.
                    self?.refreshSpace()
                    self?.accept(accepted, ignoringTemp: false)
                },
                goOn: { [weak self] in self?.accept(accepted, ignoringTemp: true) })
            return
        }
        guard let question = check?.confirm else {
            finishAccepting(accepted)
            return
        }
        let alert = NSAlert()
        alert.messageText = question
        alert.addButton(withTitle: L[.accept_anyway])
        alert.addButton(withTitle: L[.cancel])
        alert.buttons[1].keyEquivalent = "\u{1b}"
        alert.beginSheetModal(for: window) { response in
            if response == .alertFirstButtonReturn { self.finishAccepting(accepted) }
        }
    }

    private func finishAccepting(_ accepted: [UInt64]) {
        if !known, addCheck.state == .on, offer.addLabel != nil {
            // A name that is taken is not worth losing the transfer for: the
            // sender stays unknown, and can be added afterwards.
            let problem =
                Core.problemOf(
                    "check_recipient", ["name": offer.fromName, "contact_id": offer.fromId])
                ?? Core.errorOf(
                    "add_recipient", ["name": offer.fromName, "contact_id": offer.fromId])
            (NSApp.delegate as? AppDelegate)?.recipientsChanged()
            // Should something stay in this window, this is settled now.
            if problem == nil { known = true }
        }
        Core.perform("accept", ["ids": accepted, "dir": dir])
        waiting.removeAll { accepted.contains($0.id) }
        if waiting.isEmpty { answered = true }
        // Letting the offers go can let go of this window's last owner, so
        // it is kept until the rest is done.
        withExtendedLifetime(self) {
            accepted.forEach(left)
            if waiting.isEmpty {
                close()
            } else {
                // What arrived a moment before the answer stays, to be seen.
                show()
                announce(headline.stringValue, urgent: false)
            }
        }
    }

    @objc private func blockSender() {
        guard let window = window else { return }
        confirmBlocking(offer.fromName, isRecipient: known, on: window) {
            let (error, declined) = Core.block(name: self.offer.fromName, id: self.offer.fromId)
            // The library declined these offers too, and the window closes
            // with them.
            self.answered = true
            (NSApp.delegate as? AppDelegate)?.senderBlocked(
                self.offer.fromId, declined: declined, error: error)
            self.close()
        }
    }

    /// Opens the chat with the sender: the button, and Chat in the File menu.
    @objc func chat(_: Any?) {
        (NSApp.delegate as? AppDelegate)?.openChat(
            peer: offer.fromId, name: offer.fromName, activate: true)
    }

    @objc private func declineOffer() {
        window?.performClose(nil)
    }

    func windowWillClose(_: Notification) {
        settle?.cancel()
        if !answered {
            answered = true
            for id in ids { Core.perform("decline", ["id": id]) }
        }
        let gone = ids
        waiting = []
        gone.forEach(left)
    }
}
