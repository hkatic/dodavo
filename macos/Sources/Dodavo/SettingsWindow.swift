import AppKit

/// Settings apply as they are changed, as on a Mac they do.
final class SettingsWindowController: NSWindowController, NSWindowDelegate, NSTextFieldDelegate {
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

    init(started: Started, applied: @escaping () -> Void) {
        self.started = started
        self.applied = applied
        settings =
            Core.settings()
            ?? Settings(
                myName: "", openAtLogin: false, hideAtLogin: false, showInMenuBar: true,
                checkUpdates: true, language: nil, alwaysSave: false, alwaysSaveDir: nil,
                autoAccept: true, receivedFolder: "extract")
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
        tabs.addTabViewItem(general)
        tabs.addTabViewItem(receiving)
        tabs.title = general.label
        window.title = general.label
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
        nameField.setAccessibilityTitleUIElement(nameLabel)
        nameField.setAccessibilityHelp(L[.my_name_help])
        nameField.widthAnchor.constraint(greaterThanOrEqualToConstant: 280).isActive = true
        let nameHelp = makeLabel(L[.my_name_help], small: true)

        let idLabel = NSTextField(labelWithString: L[.my_id_label])
        let idText = NSTextField(labelWithString: started.myId)
        idText.isSelectable = true
        idText.font = .monospacedSystemFont(ofSize: NSFont.smallSystemFontSize, weight: .regular)
        idText.lineBreakMode = .byCharWrapping
        idText.maximumNumberOfLines = 2
        idText.preferredMaxLayoutWidth = 300
        idText.setAccessibilityTitleUIElement(idLabel)
        let copy = makeButton(L[.copy], NSApp.delegate, #selector(AppDelegate.copyMyContactID(_:)))

        let languageLabel = NSTextField(labelWithString: L[.language_label])
        language.addItem(withTitle: L[.language_system])
        language.lastItem?.representedObject = nil
        for pair in started.languages where pair.count == 2 {
            language.addItem(withTitle: pair[1])
            language.lastItem?.representedObject = pair[0]
        }
        language.target = self
        language.action = #selector(languageChanged)
        language.setAccessibilityTitleUIElement(languageLabel)

        for box in [openAtLogin, hideAtLogin, showInMenuBar, checkUpdates] {
            box.target = self
            box.action = #selector(changed)
        }
        let grid = NSGridView(views: [
            [nameLabel, nameField],
            [NSGridCell.emptyContentView, nameHelp],
            [idLabel, idText],
            [NSGridCell.emptyContentView, copy],
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
        chooseFolder.setAccessibilityHelp(L[.always_save])
        folderLabel.setAccessibilityTitleUIElement(alwaysSave)
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
        keep.setAccessibilityTitleUIElement(keepLabel)

        let grid = NSGridView(views: [
            [NSGridCell.emptyContentView, alwaysSave],
            [NSGridCell.emptyContentView, folderLabel],
            [NSGridCell.emptyContentView, chooseFolder],
            [NSGridCell.emptyContentView, autoAccept],
            [NSGridCell.emptyContentView, note],
            [keepLabel, keep],
        ])
        grid.column(at: 0).xPlacement = .trailing
        grid.rowAlignment = .firstBaseline
        grid.rowSpacing = 8
        return grid
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
        let keepIndex = keep.itemArray.firstIndex {
            ($0.representedObject as? String) == settings.receivedFolder
        }
        keep.selectItem(at: keepIndex ?? 0)
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
