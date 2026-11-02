import AppKit

/// All transfers in one window, as in the Finder's copy window. Closing it does
/// nothing to them.
final class TransfersWindowController: NSWindowController, NSTableViewDataSource,
    NSTableViewDelegate, NSWindowDelegate
{
    private var rows: [Row]
    private let table = NSTableView()
    private let emptyLine = makeLabel(L[.transfers_empty])
    private let clearButton = NSButton()

    init(rows: [Row]) {
        self.rows = rows.sorted { $0.id < $1.id }
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 560, height: 360),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered, defer: false)
        window.title = L[.transfers_title]
        window.identifier = .init("transfers")
        window.setFrameAutosaveName("Transfers")
        window.isReleasedWhenClosed = false
        super.init(window: window)
        window.delegate = self
        build(window)
        if window.frame.origin == .zero { window.center() }
    }

    required init?(coder _: NSCoder) { nil }

    private func build(_ window: NSWindow) {
        let column = NSTableColumn(identifier: .init("transfer"))
        column.resizingMask = .autoresizingMask
        table.addTableColumn(column)
        table.headerView = nil
        table.usesAutomaticRowHeights = true
        table.selectionHighlightStyle = .regular
        table.dataSource = self
        table.delegate = self
        table.columnAutoresizingStyle = .uniformColumnAutoresizingStyle
        table.setAccessibilityLabel(L[.transfers_title])

        let scroll = NSScrollView()
        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.translatesAutoresizingMaskIntoConstraints = false

        clearButton.title = L[.clear_finished]
        clearButton.bezelStyle = .rounded
        clearButton.target = self
        clearButton.action = #selector(clearFinished)
        clearButton.translatesAutoresizingMaskIntoConstraints = false

        emptyLine.alignment = .center
        emptyLine.translatesAutoresizingMaskIntoConstraints = false

        let host = NSView()
        host.addSubview(scroll)
        host.addSubview(clearButton)
        host.addSubview(emptyLine)
        NSLayoutConstraint.activate([
            scroll.topAnchor.constraint(equalTo: host.topAnchor),
            scroll.leadingAnchor.constraint(equalTo: host.leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: host.trailingAnchor),
            scroll.bottomAnchor.constraint(equalTo: clearButton.topAnchor, constant: -8),
            clearButton.trailingAnchor.constraint(equalTo: host.trailingAnchor, constant: -12),
            clearButton.bottomAnchor.constraint(equalTo: host.bottomAnchor, constant: -10),
            emptyLine.centerYAnchor.constraint(equalTo: scroll.centerYAnchor),
            emptyLine.leadingAnchor.constraint(equalTo: host.leadingAnchor, constant: 40),
            emptyLine.trailingAnchor.constraint(equalTo: host.trailingAnchor, constant: -40),
        ])
        window.contentView = host
        window.initialFirstResponder = table
        refreshEmpty()
    }

    private func refreshEmpty() {
        emptyLine.isHidden = !rows.isEmpty
        clearButton.isEnabled = rows.contains { !$0.active }
    }

    @objc private func clearFinished() {
        Core.perform("clear_finished")
    }

    /// Selects a transfer, so that VoiceOver and the eye find it.
    func reveal(_ id: UInt64) {
        guard let row = rows.firstIndex(where: { $0.id == id }) else { return }
        table.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
        table.scrollRowToVisible(row)
        window?.makeFirstResponder(table)
    }

    func update(_ row: Row) {
        if let index = rows.firstIndex(where: { $0.id == row.id }) {
            rows[index] = row
            if let cell = table.view(atColumn: 0, row: index, makeIfNecessary: false)
                as? TransferCell
            {
                cell.show(row)
                table.noteHeightOfRows(withIndexesChanged: IndexSet(integer: index))
            }
        } else {
            rows.append(row)
            rows.sort { $0.id < $1.id }
            table.reloadData()
        }
        refreshEmpty()
    }

    func remove(_ id: UInt64) {
        guard let index = rows.firstIndex(where: { $0.id == id }) else { return }
        // The focus must not go with the row.
        let hadFocus = window?.firstResponder is NSView
        rows.remove(at: index)
        table.removeRows(at: IndexSet(integer: index), withAnimation: .effectFade)
        if hadFocus { window?.makeFirstResponder(table) }
        refreshEmpty()
    }

    func numberOfRows(in _: NSTableView) -> Int {
        rows.count
    }

    func tableView(_ tableView: NSTableView, viewFor _: NSTableColumn?, row: Int) -> NSView? {
        let id = NSUserInterfaceItemIdentifier("TransferCell")
        let cell = tableView.makeView(withIdentifier: id, owner: self) as? TransferCell ?? {
            let cell = TransferCell(table: table)
            cell.identifier = id
            return cell
        }()
        cell.show(rows[row])
        return cell
    }

    /// Rows are not chosen by a click on what is in them.
    func tableView(_: NSTableView, shouldSelectRow _: Int) -> Bool {
        true
    }
}

/// One transfer: what it is, how far it is, and what can be done with it.
final class TransferCell: NSTableCellView {
    private weak var table: NSTableView?
    private var row: Row?
    private let title = NSTextField(labelWithString: "")
    private let party = makeLabel(small: true)
    private let statusLine = makeLabel()
    private let bar = NSProgressIndicator()
    private let details = makeLabel(small: true)
    private let pauseButton = NSButton()
    private let cancelButton = NSButton()
    private let openButton = NSButton()
    private let showButton = NSButton()
    private let removeButton = NSButton()
    private let mark = NSImageView()

    init(table: NSTableView) {
        self.table = table
        super.init(frame: .zero)
        title.font = .boldSystemFont(ofSize: NSFont.systemFontSize)
        title.lineBreakMode = .byTruncatingMiddle
        bar.isIndeterminate = false
        bar.minValue = 0
        bar.maxValue = 1
        bar.style = .bar
        bar.controlSize = .small
        for (button, name, action) in [
            (pauseButton, L[.pause], #selector(togglePause)),
            (cancelButton, L[.cancel_transfer], #selector(cancelTransfer)),
            (openButton, L[.open], #selector(openItem)),
            (showButton, L[.show_in_finder], #selector(showItem)),
            (removeButton, L[.remove_from_list], #selector(removeItem)),
        ] as [(NSButton, String, Selector)] {
            button.title = name
            button.bezelStyle = .rounded
            button.controlSize = .small
            button.target = self
            button.action = action
            button.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        }
        let heading = NSStackView(views: [mark, title])
        heading.spacing = 4
        let buttons = NSStackView(views: [pauseButton, cancelButton, openButton, showButton, removeButton])
        buttons.spacing = 6
        let column = makeColumn([heading, party, statusLine, bar, details, buttons], spacing: 3)
        addSubview(column)
        NSLayoutConstraint.activate([
            column.topAnchor.constraint(equalTo: topAnchor, constant: 6),
            column.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -8),
            column.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 8),
            column.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -8),
            bar.widthAnchor.constraint(equalTo: column.widthAnchor),
            statusLine.widthAnchor.constraint(equalTo: column.widthAnchor),
            details.widthAnchor.constraint(equalTo: column.widthAnchor),
        ])
        statusLine.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        details.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        // One element to VoiceOver, with its buttons a step further in, and as
        // actions from the row itself.
        setAccessibilityElement(true)
        setAccessibilityRole(.group)
    }

    required init?(coder _: NSCoder) { nil }

    func show(_ row: Row) {
        let before = self.row
        self.row = row
        guard before != row else { return }
        title.stringValue = row.item
        party.stringValue = L.f(row.sending ? .sending_to : .receiving_from, ["name": row.peer])
        statusLine.stringValue = row.status
        // Not by color alone.
        if row.active {
            mark.isHidden = true
        } else {
            mark.isHidden = false
            let symbol = row.succeeded ? "checkmark.circle" : "exclamationmark.triangle"
            mark.image = NSImage(
                systemSymbolName: symbol,
                accessibilityDescription: row.succeeded ? L[.succeeded_marker] : L[.failed_marker])
        }
        if let fraction = row.fraction {
            bar.stopAnimation(nil)
            bar.isIndeterminate = false
            bar.doubleValue = fraction
        } else {
            bar.isIndeterminate = true
            bar.startAnimation(nil)
        }
        bar.isHidden = !row.active && !row.succeeded
        bar.setAccessibilityLabel(L.f(.progress_of_item, ["item": row.item]))
        bar.setAccessibilityValue(row.amount)
        details.stringValue = [row.amount, row.speed, row.remaining, row.detail]
            .filter { !$0.isEmpty }.joined(separator: " – ")
        pauseButton.title = row.paused ? L[.resume] : L[.pause]
        arrange(pauseButton, visible: row.active, enabled: row.canPause)
        arrange(cancelButton, visible: row.active, enabled: row.canCancel)
        let saved = row.path != nil
        arrange(openButton, visible: saved, enabled: true)
        arrange(showButton, visible: saved, enabled: true)
        arrange(removeButton, visible: !row.active, enabled: true)
        setAccessibilityLabel(spoken(row))
        setAccessibilityCustomActions(customActions(row))
    }

    /// The row as one sentence.
    private func spoken(_ row: Row) -> String {
        [
            row.item,
            L.f(row.sending ? .sending_to : .receiving_from, ["name": row.peer]),
            row.status, row.amount, row.speed, row.remaining, row.detail,
        ].filter { !$0.isEmpty }.joined(separator: ", ")
    }

    private func customActions(_ row: Row) -> [NSAccessibilityCustomAction] {
        var actions: [NSAccessibilityCustomAction] = []
        func add(_ name: String, _ selector: Selector) {
            actions.append(NSAccessibilityCustomAction(name: name, target: self, selector: selector))
        }
        if row.canPause { add(row.paused ? L[.resume] : L[.pause], #selector(togglePause)) }
        if row.canCancel { add(L[.cancel_transfer], #selector(cancelTransfer)) }
        if row.path != nil {
            add(L[.open], #selector(openItem))
            add(L[.show_in_finder], #selector(showItem))
        }
        if !row.active { add(L[.remove_from_list], #selector(removeItem)) }
        return actions
    }

    /// The focus moves on before a button that has it goes away.
    private func arrange(_ button: NSButton, visible: Bool, enabled: Bool) {
        if let window = window, window.firstResponder === button, !(visible && enabled) {
            window.makeFirstResponder(table)
        }
        button.isHidden = !visible
        button.isEnabled = enabled
    }

    @objc private func togglePause() {
        guard let row = row else { return }
        Core.perform(row.paused ? "resume" : "pause", ["id": row.id])
    }

    @objc private func cancelTransfer() {
        guard let row = row, let window = window else { return }
        let alert = NSAlert()
        alert.messageText = L[.cancel_confirm]
        alert.informativeText = row.item
        alert.addButton(withTitle: L[.cancel_transfer])
        alert.addButton(withTitle: L[.keep_transferring])
        alert.buttons[1].keyEquivalent = "\u{1b}"
        alert.beginSheetModal(for: window) { response in
            if response == .alertFirstButtonReturn {
                Core.perform("cancel", ["id": row.id])
            }
        }
    }

    @objc private func openItem() {
        guard let path = row?.path else { return }
        NSWorkspace.shared.open(URL(fileURLWithPath: path))
    }

    @objc private func showItem() {
        guard let path = row?.path else { return }
        NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: path)])
    }

    @objc private func removeItem() {
        guard let row = row else { return }
        Core.perform("remove_row", ["id": row.id])
    }
}
