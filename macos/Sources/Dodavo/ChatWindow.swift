import AppKit

/// The chat with one person: the messages exchanged, and a field to write the
/// next one. One window per person, which stays out of the way when a message
/// arrives (it does not take the focus). The library keeps the conversation
/// and says what arrives; this draws it.
///
/// For VoiceOver: the conversation is a table with one message per row, which
/// the arrow keys read. A message that arrives while the window is in front
/// is announced, and one that arrives while it is not is told by a
/// notification, which VoiceOver reads, and never both. The status line also
/// says that the other person is typing and why a message could not be sent.
final class ChatWindowController: NSWindowController, NSWindowDelegate, NSTableViewDataSource,
    NSTableViewDelegate
{
    /// The other person's Contact ID, as the library knows the chat by.
    let peer: String
    private var info: ChatInfo
    private var entries: [ChatEntry]
    /// The last change of the conversation that the table shows (see `ChatInfo.seq`).
    private var seq: UInt64
    private let closed: () -> Void
    private let cameToFront: () -> Void

    private let warning = makeLabel()
    private let table = ChatTable()
    private let statusLine = makeLabel(small: true)
    private let field = NSTextField()
    private let sendButton = NSButton()
    private let addButton = NSButton()
    private let blockButton = NSButton()
    private let deleteButton = NSButton()
    private var limiter: MessageLimiter?
    private var openSheet: RecipientSheet?

    /// What arrived and has not been said yet: what arrives together is said
    /// at once.
    private var unspoken: [String] = []
    private var speaking: DispatchWorkItem?
    /// Whether the status line says that the other person is typing.
    private var showsTyping = false
    /// That the other person is typing, which came while a message waited to
    /// be said: the line shows it once that is said.
    private var typingWaiting: String?
    /// Until then VoiceOver tells of the window, which has just opened or
    /// become the key window, and an announcement made meanwhile takes the
    /// place of that, or is lost.
    private var describedBy = Date().addingTimeInterval(2)

    /// Where the next chat window opens, a step from the one before it.
    private static var nextTopLeft = NSPoint.zero

    /// The chat with `peer`; `name` is what they call themselves, for someone
    /// who is not a recipient. `cameToFront` runs when the user comes to the
    /// window, and `closed` when it closes.
    static func open(
        peer: String, name: String, cameToFront: @escaping () -> Void,
        closed: @escaping () -> Void
    ) -> ChatWindowController? {
        guard let info: ChatInfo = Core.call("chat_open", ["peer": peer, "name": name]) else {
            return nil
        }
        return ChatWindowController(info: info, cameToFront: cameToFront, closed: closed)
    }

    private init(info: ChatInfo, cameToFront: @escaping () -> Void, closed: @escaping () -> Void) {
        self.info = info
        peer = info.peer
        entries = info.entries
        seq = info.seq
        self.cameToFront = cameToFront
        self.closed = closed
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 480, height: 440),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        super.init(window: window)
        window.delegate = self
        build(window)
        showWho()
        table.reloadData()
        toLast()
        deleteButton.isEnabled = !entries.isEmpty
        window.center()
        // Each one a step from the one before, so that none hides another.
        Self.nextTopLeft = window.cascadeTopLeft(from: Self.nextTopLeft)
    }

    required init?(coder _: NSCoder) { nil }

    private func build(_ window: NSWindow) {
        warning.preferredMaxLayoutWidth = 440

        let messages = NSTableColumn(identifier: .init("message"))
        messages.resizingMask = .autoresizingMask
        table.addTableColumn(messages)
        table.headerView = nil
        table.usesAutomaticRowHeights = true
        table.columnAutoresizingStyle = .uniformColumnAutoresizingStyle
        table.allowsMultipleSelection = true
        table.dataSource = self
        table.delegate = self
        table.setAccessibilityLabel(L[.chat_history_label])
        table.onCopy = { [weak self] in self?.copySelected() }
        let scroll = NSScrollView()
        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.borderType = .bezelBorder
        scroll.translatesAutoresizingMaskIntoConstraints = false
        scroll.setContentHuggingPriority(.defaultLow, for: .vertical)

        statusLine.setAccessibilityIdentifier("status")
        // A long message is cut short here, and never widens the window:
        // the conversation has all of it.
        statusLine.lineBreakMode = .byTruncatingTail
        statusLine.maximumNumberOfLines = 1
        statusLine.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)

        let messageLabel = NSTextField(labelWithString: L[.chat_message_label])
        name(field, by: messageLabel)
        let limiter = MessageLimiter(limit: info.maxChars) { [weak self] text in
            guard let self = self else { return }
            Core.perform("chat_typing", ["peer": self.peer, "on": !text.isEmpty])
        }
        self.limiter = limiter
        field.delegate = limiter
        field.setContentHuggingPriority(.defaultLow, for: .horizontal)
        sendButton.title = L[.send_action]
        sendButton.bezelStyle = .rounded
        sendButton.target = self
        sendButton.action = #selector(sendMessage)
        // Return sends, wherever the focus is but on another button.
        sendButton.keyEquivalent = "\r"
        let writing = makeRow([messageLabel, field, sendButton])

        for (button, title, action) in [
            (addButton, L[.chat_add], #selector(addToRecipients)),
            (blockButton, L[.block], #selector(blockPerson)),
            (deleteButton, L[.chat_delete], #selector(deleteConversation)),
        ] as [(NSButton, String, Selector)] {
            button.title = title
            button.bezelStyle = .rounded
            button.target = self
            button.action = action
        }
        let buttons = makeButtonRow([], leading: [addButton, blockButton, deleteButton])

        let content = makeColumn([warning, scroll, statusLine, writing, buttons], spacing: 10)
        fill(window, with: content)
        NSLayoutConstraint.activate([
            scroll.widthAnchor.constraint(equalTo: content.widthAnchor),
            scroll.heightAnchor.constraint(greaterThanOrEqualToConstant: 160),
            statusLine.widthAnchor.constraint(equalTo: content.widthAnchor),
            writing.widthAnchor.constraint(equalTo: content.widthAnchor),
            buttons.widthAnchor.constraint(equalTo: content.widthAnchor),
            content.widthAnchor.constraint(greaterThanOrEqualToConstant: 420),
        ])
        window.setContentSize(NSSize(width: 480, height: 440))
        window.initialFirstResponder = field
        window.autorecalculatesKeyViewLoop = true
    }

    /// The title, and what the window says of a person who is no recipient.
    private func showWho() {
        window?.title = info.title
        warning.stringValue = info.warning ?? ""
        warning.isHidden = info.warning == nil
        // The focus must not stay on a button that goes away.
        if info.known, window?.firstResponder === addButton {
            window?.makeFirstResponder(field)
        }
        addButton.isHidden = info.known
    }

    // MARK: Showing it

    /// In front of Dodavo's other windows, leaving the focus where it is.
    func present() {
        window?.orderFront(nil)
    }

    func bringToFront() {
        window?.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    /// Whether the user is at this window.
    var isFront: Bool {
        NSApp.isActive && window?.isKeyWindow == true
    }

    /// Reads the chat again: its messages may have been deleted, and the
    /// person may have become a recipient.
    func reload() {
        guard let info: ChatInfo = Core.call("chat_open", ["peer": peer]) else { return }
        self.info = info
        entries = info.entries
        seq = info.seq
        unspoken = []
        showWho()
        table.reloadData()
        toLast()
        if entries.isEmpty, window?.firstResponder === deleteButton {
            window?.makeFirstResponder(field)
        }
        deleteButton.isEnabled = !entries.isEmpty
    }

    // MARK: The messages

    private static let timeOnly: DateFormatter = {
        let formatter = DateFormatter()
        formatter.dateStyle = .none
        formatter.timeStyle = .short
        return formatter
    }()

    private static let dateAndTime: DateFormatter = {
        let formatter = DateFormatter()
        formatter.dateStyle = .short
        formatter.timeStyle = .short
        return formatter
    }()

    /// Who wrote a message and when: the time of day for today, and the date
    /// with it for another day, as this Mac writes them.
    private func head(_ entry: ChatEntry) -> String {
        let date = Date(timeIntervalSince1970: TimeInterval(entry.time))
        let today = Calendar.current.isDateInToday(date)
        let time = (today ? Self.timeOnly : Self.dateAndTime).string(from: date)
        if entry.mine {
            return L.f(entry.failed ? .chat_head_failed : .chat_head_mine, ["time": time])
        }
        return L.f(.chat_head, ["name": info.name, "time": time])
    }

    /// A message as VoiceOver reads it, and as it is copied.
    private func line(_ entry: ChatEntry) -> String {
        L.f(.chat_entry, ["head": head(entry), "text": entry.text])
    }

    func numberOfRows(in _: NSTableView) -> Int {
        entries.count
    }

    func tableView(_ tableView: NSTableView, viewFor _: NSTableColumn?, row: Int) -> NSView? {
        guard entries.indices.contains(row) else { return nil }
        let id = NSUserInterfaceItemIdentifier("MessageCell")
        let cell = tableView.makeView(withIdentifier: id, owner: self) as? MessageCell ?? {
            let cell = MessageCell()
            cell.identifier = id
            return cell
        }()
        let entry = entries[row]
        cell.show(head: head(entry), text: entry.text, spoken: line(entry))
        return cell
    }

    /// The text wraps to the width of the window.
    func tableViewColumnDidResize(_: Notification) {
        guard !entries.isEmpty else { return }
        table.noteHeightOfRows(withIndexesChanged: IndexSet(integersIn: 0..<entries.count))
    }

    /// Selects the last message and brings it into view.
    private func toLast() {
        guard !entries.isEmpty else { return }
        let last = entries.count - 1
        table.selectRowIndexes(IndexSet(integer: last), byExtendingSelection: false)
        table.scrollRowToVisible(last)
    }

    /// Adds a message to the table. A user who is reading the conversation
    /// stays where they are; otherwise the new message is the selected one,
    /// so that the conversation is entered at its end.
    private func append(_ entry: ChatEntry, gone: Int) {
        let reading = window?.firstResponder === table
        table.beginUpdates()
        let leaving = min(gone, entries.count)
        if leaving > 0 {
            entries.removeFirst(leaving)
            table.removeRows(at: IndexSet(integersIn: 0..<leaving), withAnimation: [])
        }
        entries.append(entry)
        table.insertRows(at: IndexSet(integer: entries.count - 1), withAnimation: [])
        table.endUpdates()
        deleteButton.isEnabled = true
        if !reading { toLast() }
    }

    private func copySelected() {
        let lines = table.selectedRowIndexes.filter { entries.indices.contains($0) }
            .map { line(entries[$0]) }
        guard !lines.isEmpty else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(lines.joined(separator: "\n"), forType: .string)
    }

    // MARK: What the library says

    /// A message arrived (see `Message.chatMessage`).
    func received(seq: UInt64, entry: ChatEntry, gone: Int, said: String, problem: String?) {
        // A window that opened for this message has it already.
        if seq > self.seq {
            self.seq = seq
            append(entry, gone: gone)
        }
        unspoken.append(said)
        if let problem = problem { unspoken.append(problem) }
        // Whoever wrote is not typing any more.
        typingWaiting = nil
        // Said once no more has come for a moment, all at once.
        speaking?.cancel()
        let work = DispatchWorkItem { [weak self] in self?.speak() }
        speaking = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.3, execute: work)
    }

    /// A message of the user's is in the chat (see `Message.chatAdded`).
    func added(seq: UInt64, entry: ChatEntry, gone: Int, problem: String?) {
        if seq > self.seq {
            self.seq = seq
            append(entry, gone: gone)
            toLast()
        }
        if let problem = problem { say(problem, announce: true) }
    }

    /// A message of the user's was not delivered (see `Message.chatFailed`).
    func failed(seq: UInt64, index: Int?, text: String, said: String) {
        if seq > self.seq, let index = index, entries.indices.contains(index) {
            self.seq = seq
            entries[index].failed = true
            let selected = table.selectedRowIndexes
            table.reloadData(
                forRowIndexes: IndexSet(integer: index), columnIndexes: IndexSet(integer: 0))
            table.noteHeightOfRows(withIndexesChanged: IndexSet(integer: index))
            table.selectRowIndexes(selected, byExtendingSelection: false)
        }
        // Back into the field, to send it again, unless the user is writing
        // the next one.
        if field.stringValue.isEmpty { field.stringValue = text }
        say(said, announce: true)
    }

    /// The other person is typing, which `text` says, or has stopped.
    func typing(_ text: String?, announce aloud: Bool, sound: SoundInfo?) {
        guard let text = text else {
            typingWaiting = nil
            if showsTyping { say("", announce: false) }
            return
        }
        // Not over a message that is about to be said.
        guard unspoken.isEmpty else {
            typingWaiting = text
            return
        }
        let front = isFront
        say(text, announce: aloud && front)
        showsTyping = true
        if aloud, front, let sound = sound { Sounds.play(sound) }
    }

    /// Says what arrived since the last time.
    private func speak() {
        speaking = nil
        guard !unspoken.isEmpty else { return }
        let text = unspoken.joined(separator: "  ")
        unspoken = []
        say(text, announce: true)
        // Someone who went on writing meanwhile: the line says so, quietly,
        // now that the message is said. The conversation has the message.
        if let typing = typingWaiting {
            typingWaiting = nil
            statusLine.stringValue = typing
            showsTyping = true
        }
    }

    /// Puts `text` into the status line, and has VoiceOver say it when
    /// `aloud`, if the user is at this window: after VoiceOver has told of
    /// the window, and waiting its turn.
    private func say(_ text: String, announce aloud: Bool) {
        showsTyping = false
        statusLine.stringValue = text
        guard aloud, isFront, !text.isEmpty else { return }
        let wait = describedBy.timeIntervalSinceNow
        guard wait > 0 else {
            announce(text, urgent: false)
            return
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + wait) { [weak self] in
            guard let self = self, self.isFront else { return }
            announce(text, urgent: false)
        }
    }

    // MARK: Commands

    @objc private func sendMessage() {
        let text = field.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else {
            window?.makeFirstResponder(field)
            return
        }
        if let error = Core.errorOf("chat_send", ["peer": peer, "text": text]) {
            showAlert(error, on: window)
            return
        }
        field.stringValue = ""
        say("", announce: false)
        window?.makeFirstResponder(field)
    }

    @objc private func addToRecipients() {
        guard let window = window else { return }
        let proposed = Contact(name: info.name, id: info.peer, limit: nil)
        let sheet = RecipientSheet(editing: nil, proposing: proposed) { [weak self] in
            (NSApp.delegate as? AppDelegate)?.recipientsChanged()
            // With another Contact ID written into the sheet, it is another
            // person who became a recipient, and this chat stays as it is.
            self?.reload()
        }
        guard let panel = sheet.window else { return }
        openSheet = sheet
        window.beginSheet(panel) { [weak self] _ in self?.openSheet = nil }
    }

    /// Blocks the other person, if the user confirms it, which closes the chat.
    @objc private func blockPerson() {
        guard let window = window else { return }
        let (name, peer) = (info.name, info.peer)
        confirmBlocking(name, isRecipient: info.known, on: window) {
            let (error, declined) = Core.block(name: name, id: peer)
            (NSApp.delegate as? AppDelegate)?.senderBlocked(
                peer, declined: declined, error: error)
        }
    }

    @objc private func deleteConversation() {
        guard let window = window else { return }
        let alert = NSAlert()
        alert.messageText = L.f(.chat_delete_confirm, ["name": info.name])
        alert.informativeText = L[.chat_delete_text]
        alert.addButton(withTitle: L[.delete])
        alert.addButton(withTitle: L[.cancel])
        alert.buttons[1].keyEquivalent = "\u{1b}"
        alert.beginSheetModal(for: window) { [weak self] response in
            guard response == .alertFirstButtonReturn, let self = self else { return }
            if let error = Core.errorOf("chat_delete", ["peer": self.peer]) {
                showAlert(error, on: window)
                return
            }
            self.reload()
            window.makeFirstResponder(self.field)
            // Once the sheet is gone: an announcement made as it closes is lost.
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { [weak self] in
                self?.say(L[.chat_deleted], announce: true)
            }
        }
    }

    // MARK: The window

    func windowDidBecomeKey(_: Notification) {
        describedBy = Date().addingTimeInterval(2)
        cameToFront()
    }

    /// The user went elsewhere: they are not typing here now.
    func windowDidResignKey(_: Notification) {
        Core.perform("chat_typing", ["peer": peer, "on": false])
    }

    func windowWillClose(_: Notification) {
        speaking?.cancel()
        Core.perform("chat_closed", ["peer": peer])
        closed()
    }
}

/// The conversation, with Copy for the selected messages.
final class ChatTable: NSTableView {
    var onCopy: (() -> Void)?

    @objc func copy(_: Any?) {
        onCopy?()
    }

    override func validateUserInterfaceItem(_ item: NSValidatedUserInterfaceItem) -> Bool {
        if item.action == #selector(copy(_:)) { return !selectedRowIndexes.isEmpty }
        return super.validateUserInterfaceItem(item)
    }
}

/// One message: who wrote it and when, and below that what it says.
final class MessageCell: NSTableCellView {
    private let head = makeLabel(small: true)
    private let body = NSTextField(wrappingLabelWithString: "")

    init() {
        super.init(frame: .zero)
        head.lineBreakMode = .byTruncatingTail
        head.maximumNumberOfLines = 1
        body.isSelectable = false
        let column = makeColumn([head, body], spacing: 1)
        addSubview(column)
        NSLayoutConstraint.activate([
            column.topAnchor.constraint(equalTo: topAnchor, constant: 4),
            column.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -5),
            column.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 6),
            column.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -6),
            head.widthAnchor.constraint(equalTo: column.widthAnchor),
            body.widthAnchor.constraint(equalTo: column.widthAnchor),
        ])
        head.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        body.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        // To VoiceOver a message is one sentence (the cell's label), which
        // is not read a second time in its parts.
        head.cell?.setAccessibilityElement(false)
        body.cell?.setAccessibilityElement(false)
    }

    required init?(coder _: NSCoder) { nil }

    func show(head: String, text: String, spoken: String) {
        self.head.stringValue = head
        body.stringValue = text
        setAccessibilityLabel(spoken)
    }
}

/// Keeps what is written in a message field within the longest message, and
/// says when the field changes.
final class MessageLimiter: NSObject, NSTextFieldDelegate {
    private let limit: Int
    private let changed: (String) -> Void

    init(limit: Int, changed: @escaping (String) -> Void) {
        self.limit = limit
        self.changed = changed
    }

    func controlTextDidChange(_ notification: Notification) {
        guard let field = notification.object as? NSTextField else { return }
        var text = field.stringValue
        // Counted as the library counts them.
        if limit > 0, text.unicodeScalars.count > limit {
            text = String(String.UnicodeScalarView(text.unicodeScalars.prefix(limit)))
            field.stringValue = text
            NSSound.beep()
        }
        changed(text)
    }
}
