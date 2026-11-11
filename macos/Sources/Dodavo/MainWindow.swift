import AppKit

/// The list of recipients, with what is going on with each.
final class MainWindowController: NSWindowController, NSWindowDelegate, NSToolbarDelegate,
    NSTableViewDataSource, NSTableViewDelegate, NSMenuDelegate, NSMenuItemValidation
{
    private var contacts: [Contact] = []
    private var rows: [Row] = []
    private let table = RecipientsTable()
    private let statusLine = makeLabel(small: true)
    private let emptyLine = makeLabel(L[.empty_list])
    private var status = L[.status_connecting]
    private var openSheet: RecipientSheet?

    private let sendID = NSToolbarItem.Identifier("send")
    private let addID = NSToolbarItem.Identifier("add")
    private let copyID = NSToolbarItem.Identifier("copy")
    private let transfersID = NSToolbarItem.Identifier("transfers")

    init(started: Started, title: String) {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 640, height: 400),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered, defer: false)
        window.title = title
        window.setFrameAutosaveName("Recipients")
        window.isReleasedWhenClosed = false
        super.init(window: window)
        window.delegate = self
        buildToolbar(window)
        buildContent(window)
        reload()
        if window.frame.origin == .zero { window.center() }
        window.initialFirstResponder = table
        window.autorecalculatesKeyViewLoop = true
    }

    required init?(coder _: NSCoder) { nil }

    private func buildToolbar(_ window: NSWindow) {
        let toolbar = NSToolbar(identifier: "Recipients")
        toolbar.delegate = self
        toolbar.displayMode = .iconAndLabel
        toolbar.allowsUserCustomization = false
        window.toolbar = toolbar
        if #available(macOS 11, *) { window.toolbarStyle = .unified }
    }

    private func buildContent(_ window: NSWindow) {
        table.headerView = NSTableHeaderView()
        table.usesAlternatingRowBackgroundColors = true
        table.allowsMultipleSelection = false
        table.setAccessibilityLabel(L[.recipients_label])
        for (id, title, width) in [
            ("name", L[.col_name], 180.0), ("id", L[.col_contact_id], 160.0),
            ("transfer", L[.col_transfer], 260.0),
        ] as [(String, String, CGFloat)] {
            let column = NSTableColumn(identifier: .init(id))
            column.title = title
            column.width = width
            column.isEditable = false
            table.addTableColumn(column)
        }
        table.dataSource = self
        table.delegate = self
        table.target = self
        table.doubleAction = #selector(send(_:))
        table.onReturn = { [weak self] in self?.send(nil) }
        table.onDelete = { [weak self] in self?.removeRecipient(nil) }
        table.registerForDraggedTypes([.fileURL])
        let menu = NSMenu()
        menu.delegate = self
        table.menu = menu

        let scroll = NSScrollView()
        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.borderType = .noBorder
        scroll.translatesAutoresizingMaskIntoConstraints = false

        emptyLine.alignment = .center
        emptyLine.translatesAutoresizingMaskIntoConstraints = false
        statusLine.translatesAutoresizingMaskIntoConstraints = false
        statusLine.stringValue = status
        statusLine.setAccessibilityIdentifier("status")

        let host = NSView()
        host.addSubview(scroll)
        host.addSubview(emptyLine)
        host.addSubview(statusLine)
        NSLayoutConstraint.activate([
            scroll.topAnchor.constraint(equalTo: host.topAnchor),
            scroll.leadingAnchor.constraint(equalTo: host.leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: host.trailingAnchor),
            scroll.bottomAnchor.constraint(equalTo: statusLine.topAnchor, constant: -8),
            statusLine.leadingAnchor.constraint(equalTo: host.leadingAnchor, constant: 12),
            statusLine.trailingAnchor.constraint(equalTo: host.trailingAnchor, constant: -12),
            statusLine.bottomAnchor.constraint(equalTo: host.bottomAnchor, constant: -8),
            emptyLine.centerYAnchor.constraint(equalTo: scroll.centerYAnchor),
            emptyLine.leadingAnchor.constraint(equalTo: host.leadingAnchor, constant: 40),
            emptyLine.trailingAnchor.constraint(equalTo: host.trailingAnchor, constant: -40),
        ])
        window.contentView = host
    }

    // MARK: What changes

    private func reload() {
        let selected = selectedContact?.id
        contacts = Core.contacts
        table.reloadData()
        if let selected = selected, let row = contacts.firstIndex(where: { $0.id == selected }) {
            table.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
        } else if !contacts.isEmpty {
            table.selectRowIndexes(IndexSet(integer: 0), byExtendingSelection: false)
        }
        emptyLine.isHidden = !contacts.isEmpty
    }

    func recipientsChanged() {
        reload()
    }

    func setStatus(_ text: String) {
        if text != status { announce(text, urgent: false) }
        status = text
        statusLine.stringValue = text
    }

    /// While a check for updates that the user asked for is running.
    func setCheckingStatus(_ checking: Bool) {
        statusLine.stringValue = checking ? L[.update_checking] : status
        if checking { announce(L[.update_checking]) }
    }

    /// Says `text` for a moment in the status line.
    func flashStatus(_ text: String) {
        statusLine.stringValue = text
        DispatchQueue.main.asyncAfter(deadline: .now() + 4) { [weak self] in
            guard let self = self else { return }
            self.statusLine.stringValue = self.status
        }
    }

    func setRows(_ rows: [Row]) {
        self.rows = rows
        let transfer = table.column(withIdentifier: .init("transfer"))
        guard transfer >= 0, !contacts.isEmpty else { return }
        table.reloadData(
            forRowIndexes: IndexSet(integersIn: 0..<contacts.count),
            columnIndexes: IndexSet(integer: transfer))
    }

    private func transferText(of contact: Contact) -> String {
        let mine = rows.filter {
            $0.active && $0.peerId.caseInsensitiveCompare(contact.id) == .orderedSame
        }
        switch mine.count {
        case 0: return ""
        case 1: return mine[0].state
        default: return L.plural(.transfers_in_progress, mine.count)
        }
    }

    private var selectedContact: Contact? {
        let row = table.selectedRow
        return contacts.indices.contains(row) ? contacts[row] : nil
    }

    /// The contact a menu or a click means.
    private var targetContact: Contact? {
        let clicked = table.clickedRow
        return contacts.indices.contains(clicked) ? contacts[clicked] : selectedContact
    }

    // MARK: Commands

    @objc func send(_: Any?) {
        guard let contact = targetContact, let window = window else { return }
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = true
        panel.prompt = L[.send_panel_prompt]
        panel.message = L.f(.send_panel_message, ["name": contact.name])
        panel.beginSheetModal(for: window) { response in
            guard response == .OK else { return }
            self.start(contact, panel.urls.map { $0.path })
        }
    }

    private func start(_ contact: Contact, _ paths: [String]) {
        if let error = Core.errorOf("send", ["peer_id": contact.id, "paths": paths]) {
            showAlert(error, on: window)
        }
    }

    @objc func addRecipient(_: Any?) {
        presentSheet(RecipientSheet(editing: nil) { [weak self] in self?.reload() })
    }

    @objc func editRecipient(_: Any?) {
        guard let contact = targetContact else { return }
        presentSheet(RecipientSheet(editing: contact) { [weak self] in self?.reload() })
    }

    private func presentSheet(_ sheet: RecipientSheet) {
        guard let window = window, let panel = sheet.window else { return }
        openSheet = sheet
        window.beginSheet(panel) { [weak self] _ in self?.openSheet = nil }
    }

    @objc func removeRecipient(_: Any?) {
        guard let contact = targetContact, let window = window else { return }
        let alert = NSAlert()
        alert.messageText = L.f(.remove_confirm, ["name": contact.name])
        alert.informativeText = L[.remove_text]
        alert.addButton(withTitle: L[.remove])
        alert.addButton(withTitle: L[.cancel])
        alert.buttons[1].keyEquivalent = "\u{1b}"
        alert.beginSheetModal(for: window) { response in
            guard response == .alertFirstButtonReturn else { return }
            if let error = Core.errorOf("remove_recipient", ["contact_id": contact.id]) {
                showAlert(error, on: self.window)
            }
            self.reload()
        }
    }

    @objc func blockRecipient(_: Any?) {
        guard let contact = targetContact, let window = window else { return }
        confirmBlocking(contact.name, isRecipient: true, on: window) {
            let (error, declined) = Core.block(name: contact.name, id: contact.id)
            (NSApp.delegate as? AppDelegate)?.senderBlocked(declined: declined, error: error)
        }
    }

    /// Items that came from elsewhere: to whom? The selected recipient is what
    /// Return takes.
    func chooseRecipient(forSending paths: [String]) {
        guard let window = window else { return }
        reload()
        if contacts.isEmpty {
            let alert = NSAlert()
            alert.messageText = L[.no_recipients_title]
            alert.informativeText = L[.no_recipients_text]
            alert.addButton(withTitle: L[.close])
            alert.beginSheetModal(for: window, completionHandler: nil)
            return
        }
        let alert = NSAlert()
        alert.messageText = L[.pick_recipient_title]
        alert.informativeText = L[.pick_recipient_text]
        let popup = NSPopUpButton(frame: NSRect(x: 0, y: 0, width: 280, height: 26), pullsDown: false)
        popup.addItems(withTitles: contacts.map { $0.name })
        popup.selectItem(at: max(table.selectedRow, 0))
        popup.setAccessibilityLabel(L[.pick_recipient_text])
        alert.accessoryView = popup
        alert.addButton(withTitle: L[.send_action])
        alert.addButton(withTitle: L[.cancel])
        alert.buttons[1].keyEquivalent = "\u{1b}"
        alert.window.initialFirstResponder = popup
        alert.beginSheetModal(for: window) { response in
            guard response == .alertFirstButtonReturn else { return }
            self.start(self.contacts[popup.indexOfSelectedItem], paths)
        }
    }

    /// Commands that need a recipient are unavailable without one.
    @objc func validateMenuItem(_ item: NSMenuItem) -> Bool {
        switch item.action {
        case #selector(send(_:)), #selector(editRecipient(_:)), #selector(removeRecipient(_:)),
            #selector(blockRecipient(_:)):
            return targetContact != nil && window?.attachedSheet == nil
        case #selector(addRecipient(_:)):
            return window?.attachedSheet == nil
        default:
            return true
        }
    }

    // MARK: The window

    /// Closing it leaves Dodavo running, so that files can be received.
    func windowShouldClose(_ sender: NSWindow) -> Bool {
        sender.orderOut(nil)
        return false
    }

    // MARK: The toolbar

    func toolbarAllowedItemIdentifiers(_: NSToolbar) -> [NSToolbarItem.Identifier] {
        [sendID, addID, copyID, .flexibleSpace, transfersID]
    }

    func toolbarDefaultItemIdentifiers(_: NSToolbar) -> [NSToolbarItem.Identifier] {
        [sendID, addID, copyID, .flexibleSpace, transfersID]
    }

    func toolbar(
        _: NSToolbar, itemForItemIdentifier id: NSToolbarItem.Identifier,
        willBeInsertedIntoToolbar _: Bool
    ) -> NSToolbarItem? {
        let item = NSToolbarItem(itemIdentifier: id)
        func set(_ label: String, _ symbol: String, _ target: AnyObject, _ action: Selector) {
            item.label = label
            item.paletteLabel = label
            item.toolTip = label
            item.image = NSImage(systemSymbolName: symbol, accessibilityDescription: label)
            item.target = target
            item.action = action
            item.isBordered = true
        }
        let app = NSApp.delegate as AnyObject
        switch id {
        case sendID: set(L[.toolbar_send], "paperplane", self, #selector(send(_:)))
        case addID: set(L[.toolbar_add], "person.badge.plus", self, #selector(addRecipient(_:)))
        case copyID:
            set(L[.toolbar_copy_id], "doc.on.doc", app, #selector(AppDelegate.copyMyContactID(_:)))
        case transfersID:
            set(
                L[.toolbar_transfers], "arrow.up.arrow.down", app,
                #selector(AppDelegate.showTransfers(_:)))
        default: return nil
        }
        return item
    }

    @objc func validateToolbarItem(_ item: NSToolbarItem) -> Bool {
        item.itemIdentifier == sendID ? targetContact != nil : true
    }

    // MARK: The table

    func numberOfRows(in _: NSTableView) -> Int {
        contacts.count
    }

    func tableView(_ tableView: NSTableView, viewFor column: NSTableColumn?, row: Int) -> NSView? {
        guard let id = column?.identifier, contacts.indices.contains(row) else { return nil }
        let cell =
            tableView.makeView(withIdentifier: id, owner: self) as? NSTableCellView
            ?? makeCell(id)
        let contact = contacts[row]
        switch id.rawValue {
        case "name":
            cell.textField?.stringValue = contact.name
        case "id":
            let shortened =
                contact.id.count <= 16
                ? contact.id : "\(contact.id.prefix(8))…\(contact.id.suffix(6))"
            cell.textField?.stringValue = shortened
            cell.textField?.toolTip = contact.id
        default:
            cell.textField?.stringValue = transferText(of: contact)
        }
        return cell
    }

    private func makeCell(_ id: NSUserInterfaceItemIdentifier) -> NSTableCellView {
        let cell = NSTableCellView()
        cell.identifier = id
        let text = NSTextField(labelWithString: "")
        text.lineBreakMode = .byTruncatingTail
        text.translatesAutoresizingMaskIntoConstraints = false
        cell.addSubview(text)
        cell.textField = text
        NSLayoutConstraint.activate([
            text.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 2),
            text.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -2),
            text.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
        ])
        return cell
    }

    func menuNeedsUpdate(_ menu: NSMenu) {
        menu.removeAllItems()
        func add(_ title: String, _ action: Selector) {
            let item = menu.addItem(withTitle: title, action: action, keyEquivalent: "")
            item.target = self
        }
        guard let contact = targetContact else {
            add(L[.menu_add_recipient], #selector(addRecipient(_:)))
            return
        }
        add(L[.menu_send], #selector(send(_:)))
        if rows.contains(where: {
            $0.active && $0.peerId.caseInsensitiveCompare(contact.id) == .orderedSame
        }) {
            let show = menu.addItem(
                withTitle: L[.menu_show_transfer],
                action: #selector(AppDelegate.showTransfers(_:)), keyEquivalent: "")
            show.target = NSApp.delegate
        }
        menu.addItem(.separator())
        add(L[.menu_edit_recipient], #selector(editRecipient(_:)))
        add(L[.menu_remove_recipient], #selector(removeRecipient(_:)))
        add(L[.menu_block_recipient], #selector(blockRecipient(_:)))
    }

    // MARK: Dropping items on a recipient

    func tableView(
        _: NSTableView, validateDrop info: NSDraggingInfo, proposedRow row: Int,
        proposedDropOperation operation: NSTableView.DropOperation
    ) -> NSDragOperation {
        guard !contacts.isEmpty else { return [] }
        if operation == .above || !contacts.indices.contains(row) {
            // Dropped between rows or below them: on the selected one.
            let selected = max(table.selectedRow, 0)
            table.setDropRow(selected, dropOperation: .on)
        }
        return .copy
    }

    func tableView(
        _: NSTableView, acceptDrop info: NSDraggingInfo, row: Int,
        dropOperation _: NSTableView.DropOperation
    ) -> Bool {
        let urls =
            info.draggingPasteboard.readObjects(
                forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true]) as? [URL] ?? []
        guard !urls.isEmpty, contacts.indices.contains(row), let window = window else {
            return false
        }
        let contact = contacts[row]
        table.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
        let paths = urls.map { $0.path }
        if paths.count == 1 {
            start(contact, paths)
            return true
        }
        let alert = NSAlert()
        alert.messageText = L.plural(.drop_confirm_many, paths.count, ["name": contact.name])
        alert.addButton(withTitle: L[.send_action])
        alert.addButton(withTitle: L[.cancel])
        alert.buttons[1].keyEquivalent = "\u{1b}"
        alert.beginSheetModal(for: window) { response in
            if response == .alertFirstButtonReturn { self.start(contact, paths) }
        }
        return true
    }
}

/// The table, with the keys that it takes.
final class RecipientsTable: NSTableView {
    var onReturn: (() -> Void)?
    var onDelete: (() -> Void)?

    override func keyDown(with event: NSEvent) {
        switch event.keyCode {
        case 36, 76:  // Return and Enter.
            onReturn?()
        case 51 where event.modifierFlags.intersection(.deviceIndependentFlagsMask).isEmpty:  // Delete.
            onDelete?()
        default:
            super.keyDown(with: event)
        }
    }
}

/// Add Recipient and Edit Recipient, as a sheet.
final class RecipientSheet: NSWindowController {
    private let editing: Contact?
    private let done: () -> Void
    private let nameField = NSTextField()
    private let idField = NSTextField()
    /// Which limit holds for the recipient, and the size of one of their own.
    private let limitKind = NSPopUpButton()
    private let limitNumber = NSTextField()
    private let limitUnit = NSPopUpButton()

    init(editing: Contact?, done: @escaping () -> Void) {
        self.editing = editing
        self.done = done
        let panel = NSPanel(
            contentRect: NSRect(x: 0, y: 0, width: 460, height: 200),
            styleMask: [.titled], backing: .buffered, defer: false)
        panel.title = editing == nil ? L[.add_recipient_title] : L[.edit_recipient_title]
        super.init(window: panel)
        build(panel)
    }

    required init?(coder _: NSCoder) { nil }

    private func build(_ panel: NSPanel) {
        let heading = makeLabel(panel.title, bold: true)
        let nameLabel = NSTextField(labelWithString: L[.recipient_name_label])
        let idLabel = NSTextField(labelWithString: L[.contact_id_label])
        nameField.stringValue = editing?.name ?? ""
        idField.stringValue = editing?.id ?? ""
        nameField.placeholderString = L[.recipient_name_help]
        name(nameField, by: nameLabel)
        name(idField, by: idLabel)
        nameField.setAccessibilityHelp(L[.recipient_name_help])
        idField.setAccessibilityHelp(L[.recipient_hint])

        let limitLabel = NSTextField(labelWithString: L[.recipient_limit_label])
        for (title, value) in [
            (L[.recipient_limit_general], "general"), (L[.recipient_limit_none], "none"),
            (L[.recipient_limit_own], "own"),
        ] {
            limitKind.addItem(withTitle: title)
            limitKind.lastItem?.representedObject = value
        }
        limitKind.target = self
        limitKind.action = #selector(limitChanged)
        name(limitKind, by: limitLabel)
        limitKind.setAccessibilityHelp(L[.recipient_limit_help])
        limitKind.toolTip = L[.recipient_limit_help]
        for (title, value) in [(L[.unit_mb], "mb"), (L[.unit_gb], "gb")] {
            limitUnit.addItem(withTitle: title)
            limitUnit.lastItem?.representedObject = value
        }
        limitUnit.setAccessibilityLabel(L[.limit_unit_name])
        var sizeName = L[.limit_size_label]
        if sizeName.hasSuffix(":") { sizeName.removeLast() }
        limitNumber.setAccessibilityLabel(sizeName)
        limitNumber.alignment = .right
        limitNumber.widthAnchor.constraint(equalToConstant: 80).isActive = true
        // A limit of their own starts from the one in the settings.
        let general = Core.settings()
        let limit = editing?.limit
        let kindIndex = limitKind.itemArray.firstIndex {
            ($0.representedObject as? String) == limit?.kind
        }
        limitKind.selectItem(at: kindIndex ?? 0)
        limitNumber.stringValue = limit?.number ?? general?.limitNumber ?? "1"
        let unit = limit?.unit ?? general?.limitUnit ?? "gb"
        let unitIndex = limitUnit.itemArray.firstIndex {
            ($0.representedObject as? String) == unit
        }
        limitUnit.selectItem(at: unitIndex ?? 0)
        limitChanged()

        let grid = NSGridView(views: [
            [nameLabel, nameField], [idLabel, idField],
            [limitLabel, makeRow([limitKind, limitNumber, limitUnit])],
        ])
        grid.column(at: 0).xPlacement = .trailing
        grid.rowAlignment = .firstBaseline
        grid.translatesAutoresizingMaskIntoConstraints = false
        nameField.widthAnchor.constraint(greaterThanOrEqualToConstant: 320).isActive = true
        let hint = makeLabel(L[.recipient_hint], small: true)
        hint.preferredMaxLayoutWidth = 420
        let cancel = makeButton(L[.cancel], self, #selector(cancelSheet))
        cancel.keyEquivalent = "\u{1b}"
        let ok = makeButton(editing == nil ? L[.add] : L[.save], self, #selector(confirm))
        ok.keyEquivalent = "\r"
        let buttons = makeButtonRow([cancel, ok])
        let column = makeColumn([heading, grid, hint, buttons], spacing: 12)
        fill(panel, with: column)
        buttons.widthAnchor.constraint(equalTo: column.widthAnchor).isActive = true
        panel.initialFirstResponder = nameField
        nameField.nextKeyView = idField
        idField.nextKeyView = limitKind
        limitKind.nextKeyView = limitNumber
        limitNumber.nextKeyView = limitUnit
        limitUnit.nextKeyView = cancel
        cancel.nextKeyView = ok
        ok.nextKeyView = nameField
    }

    @objc private func cancelSheet() {
        guard let panel = window else { return }
        panel.sheetParent?.endSheet(panel)
    }

    /// The size and its unit are for a limit of the recipient's own.
    @objc private func limitChanged() {
        let own = limitKind.selectedItem?.representedObject as? String == "own"
        limitNumber.isEnabled = own
        limitUnit.isEnabled = own
    }

    @objc private func confirm() {
        guard let panel = window else { return }
        let name = nameField.stringValue
        let id = idField.stringValue
        let limit: [String: Any] = [
            "kind": limitKind.selectedItem?.representedObject as? String ?? "general",
            "number": limitNumber.stringValue,
            "unit": limitUnit.selectedItem?.representedObject as? String ?? "mb",
        ]
        var arguments: [String: Any] = ["name": name, "contact_id": id, "limit": limit]
        if let old = editing?.id { arguments["editing"] = old }
        if let problem = Core.problemOf("check_recipient", arguments) {
            showAlert(problem, on: panel)
            return
        }
        let error: String?
        if let old = editing?.id {
            error = Core.errorOf(
                "edit_recipient",
                ["old_id": old, "name": name, "contact_id": id, "limit": limit])
        } else {
            error = Core.errorOf(
                "add_recipient", ["name": name, "contact_id": id, "limit": limit])
        }
        if let error = error {
            showAlert(error, on: panel)
            return
        }
        done()
        panel.sheetParent?.endSheet(panel)
    }
}
