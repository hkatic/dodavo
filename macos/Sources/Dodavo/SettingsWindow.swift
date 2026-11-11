import AppKit

/// Settings apply as they are changed, as on a Mac they do.
final class SettingsWindowController: NSWindowController, NSWindowDelegate, NSTextFieldDelegate,
    NSTableViewDataSource, NSTableViewDelegate
{
    private var settings: Settings
    private let started: Started
    private let applied: () -> Void

    private let nameField = NSTextField()
    private let openAtLogin = NSButton(checkboxWithTitle: L[.open_at_login], target: nil, action: nil)
    private let hideAtLogin = NSButton(checkboxWithTitle: L[.hide_at_login], target: nil, action: nil)
    private let showInMenuBar = NSButton(checkboxWithTitle: L[.show_in_menu_bar], target: nil, action: nil)
    private let checkUpdates = NSButton(checkboxWithTitle: L[.check_updates], target: nil, action: nil)
    private let language = NSPopUpButton()
    private let alwaysSave = NSButton(checkboxWithTitle: L[.always_save], target: nil, action: nil)
    private let folderLabel = NSTextField(wrappingLabelWithString: "")
    private let chooseFolder = NSButton()
    private let autoAccept = NSButton(checkboxWithTitle: L[.auto_accept], target: nil, action: nil)
    private let keep = NSPopUpButton()
    private let limitSize = NSButton(checkboxWithTitle: L[.limit_size], target: nil, action: nil)
    private let limitNumber = NSTextField()
    private let limitUnit = NSPopUpButton()
    private let overLimit = NSPopUpButton()
    private let idText = NSTextField(labelWithString: "")
    private let blockedTable = NSTableView()
    private let blockedNone = makeLabel(L[.blocked_none])
    private let unblock = NSButton()
    /// What the table of blocked senders shows.
    private var blocked: [Contact] = []

    init(started: Started, applied: @escaping () -> Void) {
        self.started = started
        self.applied = applied
        settings =
            Core.settings()
            ?? Settings(
                myName: "", openAtLogin: false, hideAtLogin: false, showInMenuBar: true,
                checkUpdates: true, language: nil, alwaysSave: true, alwaysSaveDir: nil,
                autoAccept: true, receivedFolder: "extract", limitSize: false,
                limitNumber: "1", limitUnit: "gb", overLimit: "ask")
        settings.openAtLogin = LoginItem.isEnabled ?? settings.openAtLogin
        let tabs = NSTabViewController()
        tabs.tabStyle = .toolbar
        let window = NSWindow(contentViewController: tabs)
        window.styleMask = [.titled, .closable]
        window.toolbarStyle = .preference
        window.isReleasedWhenClosed = false
        super.init(window: window)
        window.delegate = self
        let general = page(L[.settings_general], "gearshape", generalPage())
        let receiving = page(L[.settings_receiving], "square.and.arrow.down", receivingPage())
        let blockedTab = page(L[.settings_blocked], "hand.raised", blockedPage())
        tabs.addTabViewItem(general)
        tabs.addTabViewItem(receiving)
        tabs.addTabViewItem(blockedTab)
        show()
        window.setContentSize(tabs.view.fittingSize)
        window.center()
    }

    required init?(coder _: NSCoder) { nil }

    private func page(_ title: String, _ symbol: String, _ view: NSView) -> NSTabViewItem {
        let controller = NSViewController()
        // The window takes its title from the page that shows.
        controller.title = title
        let host = NSView()
        view.translatesAutoresizingMaskIntoConstraints = false
        host.addSubview(view)
        NSLayoutConstraint.activate([
            view.topAnchor.constraint(equalTo: host.topAnchor, constant: 20),
            view.leadingAnchor.constraint(equalTo: host.leadingAnchor, constant: 20),
            view.trailingAnchor.constraint(equalTo: host.trailingAnchor, constant: -20),
            // At the top: the window fits the tallest page.
            view.bottomAnchor.constraint(lessThanOrEqualTo: host.bottomAnchor, constant: -20),
            host.widthAnchor.constraint(greaterThanOrEqualToConstant: 480),
        ])
        controller.view = host
        let item = NSTabViewItem(viewController: controller)
        item.label = title
        item.image = NSImage(systemSymbolName: symbol, accessibilityDescription: title)
        return item
    }

    private func generalPage() -> NSView {
        let nameLabel = NSTextField(labelWithString: L[.my_name_label])
        nameField.delegate = self
        name(nameField, by: nameLabel)
        nameField.setAccessibilityHelp(L[.my_name_help])
        nameField.widthAnchor.constraint(greaterThanOrEqualToConstant: 280).isActive = true
        let nameHelp = makeLabel(L[.my_name_help], small: true)

        let idLabel = NSTextField(labelWithString: L[.my_id_label])
        idText.stringValue = (NSApp.delegate as? AppDelegate)?.myId ?? started.myId
        idText.isSelectable = true
        idText.font = .monospacedSystemFont(ofSize: NSFont.smallSystemFontSize, weight: .regular)
        idText.lineBreakMode = .byCharWrapping
        idText.maximumNumberOfLines = 2
        idText.preferredMaxLayoutWidth = 300
        name(idText, by: idLabel)
        let copy = makeButton(L[.copy], NSApp.delegate, #selector(AppDelegate.copyMyContactID(_:)))
        // Applies at once, like blocking: it is not one of the settings.
        let change = makeButton(
            L[.change_id_button], NSApp.delegate, #selector(AppDelegate.changeMyContactID(_:)))
        // "Change…" alone does not say what.
        change.setAccessibilityTitle("\(idLabel.stringValue) \(L[.change_id_button])")

        let languageLabel = NSTextField(labelWithString: L[.language_label])
        language.addItem(withTitle: L[.language_system])
        language.lastItem?.representedObject = nil
        for pair in started.languages where pair.count == 2 {
            language.addItem(withTitle: pair[1])
            language.lastItem?.representedObject = pair[0]
        }
        language.target = self
        language.action = #selector(languageChanged)
        name(language, by: languageLabel)

        for box in [openAtLogin, hideAtLogin, showInMenuBar, checkUpdates] {
            box.target = self
            box.action = #selector(changed)
        }
        let grid = NSGridView(views: [
            [nameLabel, nameField],
            [NSGridCell.emptyContentView, nameHelp],
            [idLabel, idText],
            [NSGridCell.emptyContentView, makeRow([copy, change])],
            [NSGridCell.emptyContentView, openAtLogin],
            [NSGridCell.emptyContentView, hideAtLogin],
            [NSGridCell.emptyContentView, showInMenuBar],
            [NSGridCell.emptyContentView, checkUpdates],
            [languageLabel, language],
        ])
        grid.column(at: 0).xPlacement = .trailing
        grid.rowAlignment = .firstBaseline
        grid.rowSpacing = 8
        return grid
    }

    private func receivingPage() -> NSView {
        alwaysSave.target = self
        alwaysSave.action = #selector(changed)
        autoAccept.target = self
        autoAccept.action = #selector(changed)
        folderLabel.isSelectable = false
        folderLabel.preferredMaxLayoutWidth = 300
        chooseFolder.title = L[.choose]
        chooseFolder.bezelStyle = .rounded
        chooseFolder.target = self
        chooseFolder.action = #selector(pickFolder)
        chooseFolder.setAccessibilityTitle("\(L[.always_save]): \(L[.choose])")
        folderLabel.setAccessibilityLabel(L[.always_save])
        let note = makeLabel(L[.auto_accept_note], small: true)
        note.preferredMaxLayoutWidth = 400

        let keepLabel = NSTextField(labelWithString: L[.received_folder_label])
        for (title, value) in [
            (L[.received_extract], "extract"), (L[.received_zip], "keep_zip"),
            (L[.received_both], "both"),
        ] {
            keep.addItem(withTitle: title)
            keep.lastItem?.representedObject = value
        }
        keep.target = self
        keep.action = #selector(changed)
        name(keep, by: keepLabel)

        limitSize.target = self
        limitSize.action = #selector(changed)
        let sizeLabel = NSTextField(labelWithString: L[.limit_size_label])
        limitNumber.delegate = self
        limitNumber.alignment = .right
        limitNumber.widthAnchor.constraint(equalToConstant: 80).isActive = true
        name(limitNumber, by: sizeLabel)
        for (title, value) in [(L[.unit_mb], "mb"), (L[.unit_gb], "gb")] {
            limitUnit.addItem(withTitle: title)
            limitUnit.lastItem?.representedObject = value
        }
        limitUnit.target = self
        limitUnit.action = #selector(changed)
        limitUnit.setAccessibilityLabel(L[.limit_unit_name])
        let overLabel = NSTextField(labelWithString: L[.over_limit_label])
        for (title, value) in [
            (L[.over_ask], "ask"), (L[.over_decline], "decline"),
            (L[.over_decline_tell], "decline_and_tell"),
        ] {
            overLimit.addItem(withTitle: title)
            overLimit.lastItem?.representedObject = value
        }
        overLimit.target = self
        overLimit.action = #selector(changed)
        name(overLimit, by: overLabel)

        let grid = NSGridView(views: [
            [NSGridCell.emptyContentView, alwaysSave],
            [NSGridCell.emptyContentView, folderLabel],
            [NSGridCell.emptyContentView, chooseFolder],
            [NSGridCell.emptyContentView, autoAccept],
            [NSGridCell.emptyContentView, note],
            [keepLabel, keep],
            [NSGridCell.emptyContentView, limitSize],
            [sizeLabel, makeRow([limitNumber, limitUnit])],
            [overLabel, overLimit],
        ])
        grid.column(at: 0).xPlacement = .trailing
        grid.rowAlignment = .firstBaseline
        grid.rowSpacing = 8
        return grid
    }

    private func blockedPage() -> NSView {
        let label = NSTextField(labelWithString: L[.blocked_senders_label])
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("sender"))
        column.resizingMask = .autoresizingMask
        blockedTable.addTableColumn(column)
        blockedTable.headerView = nil
        blockedTable.dataSource = self
        blockedTable.delegate = self
        name(blockedTable, by: label)
        let scroll = NSScrollView()
        scroll.documentView = blockedTable
        scroll.hasVerticalScroller = true
        scroll.borderType = .bezelBorder
        scroll.translatesAutoresizingMaskIntoConstraints = false
        NSLayoutConstraint.activate([
            scroll.widthAnchor.constraint(equalToConstant: 440),
            scroll.heightAnchor.constraint(equalToConstant: 150),
        ])
        unblock.title = L[.unblock]
        unblock.bezelStyle = .rounded
        unblock.target = self
        unblock.action = #selector(unblockSelected)
        blockedNone.preferredMaxLayoutWidth = 440
        reloadBlocked()
        return makeColumn([label, scroll, blockedNone, unblock])
    }

    /// Someone was blocked elsewhere (see `AppDelegate.senderBlocked`).
    func blockedChanged() {
        reloadBlocked()
    }

    /// The Contact ID was changed (see `AppDelegate.changeMyContactID`).
    func contactIDChanged(_ id: String) {
        idText.stringValue = id
    }

    private func reloadBlocked() {
        let selected = blockedTable.selectedRow
        blocked = Core.blocked()
        blockedTable.reloadData()
        blockedNone.isHidden = !blocked.isEmpty
        if !blocked.isEmpty {
            let row = min(max(selected, 0), blocked.count - 1)
            blockedTable.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
        }
        unblock.isEnabled = blockedTable.selectedRow >= 0
    }

    @objc private func unblockSelected() {
        let row = blockedTable.selectedRow
        guard blocked.indices.contains(row) else { return }
        if let error = Core.errorOf("unblock", ["contact_id": blocked[row].id]) {
            showAlert(error, on: window)
        }
        reloadBlocked()
        // The button may be unavailable now, and the focus must not stay on it.
        window?.makeFirstResponder(blockedTable)
        if blocked.isEmpty {
            // After VoiceOver has begun to tell where the focus went, which
            // would take the place of this, and waiting for it to finish.
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { [weak self] in
                // Unless the user has left the list meanwhile (the window
                // closed, another page), or someone was blocked.
                guard let self = self, self.blocked.isEmpty,
                    self.blockedTable.window?.isKeyWindow == true
                else { return }
                announce(L[.blocked_none], urgent: false)
            }
        }
    }

    func numberOfRows(in _: NSTableView) -> Int {
        blocked.count
    }

    func tableView(_ tableView: NSTableView, viewFor column: NSTableColumn?, row: Int) -> NSView? {
        guard blocked.indices.contains(row) else { return nil }
        let id = NSUserInterfaceItemIdentifier("sender")
        let cell: NSTableCellView
        if let reused = tableView.makeView(withIdentifier: id, owner: self) as? NSTableCellView {
            cell = reused
        } else {
            cell = NSTableCellView()
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
        }
        let sender = blocked[row]
        let shortened =
            sender.id.count <= 16 ? sender.id : "\(sender.id.prefix(8))…\(sender.id.suffix(6))"
        cell.textField?.stringValue = L.f(.blocked_entry, ["name": sender.name, "id": shortened])
        cell.textField?.toolTip = sender.id
        return cell
    }

    func tableViewSelectionDidChange(_: Notification) {
        unblock.isEnabled = blockedTable.selectedRow >= 0
    }

    private func show() {
        nameField.stringValue = settings.myName
        openAtLogin.state = settings.openAtLogin ? .on : .off
        hideAtLogin.state = settings.hideAtLogin ? .on : .off
        hideAtLogin.isEnabled = settings.openAtLogin
        showInMenuBar.state = settings.showInMenuBar ? .on : .off
        checkUpdates.state = settings.checkUpdates ? .on : .off
        let languageIndex = language.itemArray.firstIndex {
            ($0.representedObject as? String) == settings.language
        }
        language.selectItem(at: languageIndex ?? 0)
        alwaysSave.state = settings.alwaysSave ? .on : .off
        folderLabel.stringValue = settings.alwaysSaveDir ?? ""
        folderLabel.isHidden = settings.alwaysSaveDir == nil
        chooseFolder.isEnabled = settings.alwaysSave
        autoAccept.state = settings.autoAccept ? .on : .off
        // Without the folder Dodavo asks where to save, so nothing can start by itself.
        autoAccept.isEnabled = settings.alwaysSave
        let keepIndex = keep.itemArray.firstIndex {
            ($0.representedObject as? String) == settings.receivedFolder
        }
        keep.selectItem(at: keepIndex ?? 0)
        limitSize.state = settings.limitSize ? .on : .off
        limitNumber.stringValue = settings.limitNumber
        // The size is for the limit. What happens to a larger item stays: it
        // also holds for the limits that recipients have of their own.
        limitNumber.isEnabled = settings.limitSize
        limitUnit.isEnabled = settings.limitSize
        let unitIndex = limitUnit.itemArray.firstIndex {
            ($0.representedObject as? String) == settings.limitUnit
        }
        limitUnit.selectItem(at: unitIndex ?? 0)
        let overIndex = overLimit.itemArray.firstIndex {
            ($0.representedObject as? String) == settings.overLimit
        }
        overLimit.selectItem(at: overIndex ?? 0)
    }

    /// Takes what the window says, and saves it where it can be used.
    private func commit() {
        let wasOpenAtLogin = settings.openAtLogin
        var next = settings
        next.myName = nameField.stringValue
        next.openAtLogin = openAtLogin.state == .on
        next.hideAtLogin = hideAtLogin.state == .on
        next.showInMenuBar = showInMenuBar.state == .on
        next.checkUpdates = checkUpdates.state == .on
        next.alwaysSave = alwaysSave.state == .on
        next.autoAccept = autoAccept.state == .on
        next.receivedFolder = keep.selectedItem?.representedObject as? String ?? "extract"
        next.limitSize = limitSize.state == .on
        next.limitNumber = limitNumber.stringValue
        next.limitUnit = limitUnit.selectedItem?.representedObject as? String ?? "mb"
        next.overLimit = overLimit.selectedItem?.representedObject as? String ?? "ask"
        if next.alwaysSave, next.alwaysSaveDir == nil {
            // Nothing to save to yet: the panel comes first.
            alwaysSave.state = .off
            pickFolder()
            return
        }
        if let problem = Core.problemOf("settings_problem", Core.settingsJSON(next)) {
            showAlert(problem, on: window)
            show()
            return
        }
        if next.openAtLogin != wasOpenAtLogin, let error = LoginItem.set(next.openAtLogin) {
            showAlert(L.f(.login_item_failed, ["error": error]), on: window)
            next.openAtLogin = wasOpenAtLogin
        }
        settings = next
        if let error = Core.errorOf("set_settings", Core.settingsJSON(next)) {
            showAlert(error, on: window)
        }
        show()
        applied()
    }

    @objc private func changed() {
        commit()
    }

    func controlTextDidEndEditing(_: Notification) {
        commit()
    }

    func windowWillClose(_: Notification) {
        window?.makeFirstResponder(nil)
    }

    /// Adding a recipient elsewhere may have unblocked them.
    func windowDidBecomeKey(_: Notification) {
        reloadBlocked()
    }

    @objc private func languageChanged() {
        settings.language = language.selectedItem?.representedObject as? String
        // What the system draws for the app follows this as well.
        if let code = settings.language {
            UserDefaults.standard.set([code], forKey: "AppleLanguages")
        } else {
            UserDefaults.standard.removeObject(forKey: "AppleLanguages")
        }
        commit()
        guard let window = window else { return }
        let alert = NSAlert()
        alert.messageText = L[.language_restart]
        alert.addButton(withTitle: L[.close])
        alert.beginSheetModal(for: window, completionHandler: nil)
    }

    @objc private func pickFolder() {
        guard let window = window else { return }
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.canCreateDirectories = true
        panel.prompt = L[.choose_folder_prompt]
        if let dir = settings.alwaysSaveDir {
            panel.directoryURL = URL(fileURLWithPath: dir)
        }
        panel.beginSheetModal(for: window) { response in
            guard response == .OK, let url = panel.url else { return }
            self.settings.alwaysSaveDir = url.path
            self.alwaysSave.state = .on
            self.commit()
        }
    }
}
