import AppKit
import UniformTypeIdentifiers

/// The Sounds page of the settings: whether sounds play, and what each event
/// plays. As everywhere in the settings, a change applies at once.
final class SoundsPane: NSObject, NSTableViewDataSource, NSTableViewDelegate {
    let view: NSView
    private let on = NSButton(checkboxWithTitle: L[.sounds_on], target: nil, action: nil)
    private let table = NSTableView()
    private let choice = NSPopUpButton()
    private let play = NSButton()
    private let reset = NSButton()
    private var events: [SoundSetting] = []

    override init() {
        let eventsLabel = NSTextField(labelWithString: L[.sounds_events_label])
        for (id, title, width) in [
            ("event", L[.col_event], 250.0), ("sound", L[.col_sound], 170.0),
        ] as [(String, String, CGFloat)] {
            let column = NSTableColumn(identifier: .init(id))
            column.title = title
            column.width = width
            column.isEditable = false
            table.addTableColumn(column)
        }
        table.allowsEmptySelection = false
        table.allowsMultipleSelection = false
        table.usesAlternatingRowBackgroundColors = true
        name(table, by: eventsLabel)
        let scroll = NSScrollView()
        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.borderType = .bezelBorder
        scroll.translatesAutoresizingMaskIntoConstraints = false
        NSLayoutConstraint.activate([
            scroll.widthAnchor.constraint(equalToConstant: 440),
            scroll.heightAnchor.constraint(equalToConstant: 296),
        ])

        let choiceLabel = NSTextField(labelWithString: L[.sound_choice_label])
        name(choice, by: choiceLabel)
        play.title = L[.sound_play]
        play.bezelStyle = .rounded
        reset.title = L[.sounds_reset]
        reset.bezelStyle = .rounded
        view = makeColumn([
            on, eventsLabel, scroll, makeRow([choiceLabel, choice, play]), reset,
        ])
        super.init()
        table.dataSource = self
        table.delegate = self
        on.target = self
        on.action = #selector(onChanged)
        choice.target = self
        choice.action = #selector(choiceChanged)
        play.target = self
        play.action = #selector(playSelected)
        reset.target = self
        reset.action = #selector(resetAll)
        reload()
    }

    private var selected: SoundSetting? {
        events.indices.contains(table.selectedRow) ? events[table.selectedRow] : nil
    }

    /// Reads the settings again, keeping the event that is selected.
    private func reload() {
        guard let settings: SoundSettings = Core.call("sounds") else { return }
        let row = max(table.selectedRow, 0)
        events = settings.events
        on.state = settings.on ? .on : .off
        table.reloadData()
        if !events.isEmpty {
            table.selectRowIndexes(
                IndexSet(integer: min(row, events.count - 1)), byExtendingSelection: false)
        }
        showChoice()
    }

    /// Shows what the selected event plays in the controls below the table.
    private func showChoice() {
        choice.removeAllItems()
        guard let event = selected, let menu = choice.menu else {
            choice.isEnabled = false
            play.isEnabled = false
            return
        }
        func add(_ title: String, _ kind: String) {
            let item = NSMenuItem(title: title, action: nil, keyEquivalent: "")
            item.representedObject = kind
            menu.addItem(item)
        }
        add(L[.sound_choice_default], "default")
        add(L[.sound_choice_off], "off")
        // A file of the user's, once one is chosen, by its name.
        if event.file != nil {
            add(event.shown, "file")
            menu.items.last?.toolTip = event.file
        }
        menu.addItem(.separator())
        add(L[.choose], "choose")
        let index = menu.items.firstIndex { ($0.representedObject as? String) == event.kind }
        choice.selectItem(at: index ?? 0)
        choice.isEnabled = true
        // The focus must not stay on a button that cannot be used.
        let silent = event.kind == "off"
        if silent, play.window?.firstResponder === play {
            play.window?.makeFirstResponder(choice)
        }
        play.isEnabled = !silent
    }

    private func set(_ event: SoundSetting, kind: String, file: String?) {
        var arguments: [String: Any] = ["event": event.event, "kind": kind]
        if let file = file { arguments["file"] = file }
        if let error = Core.errorOf("set_sound", arguments) {
            showAlert(error, on: view.window)
        }
        reload()
    }

    @objc private func onChanged() {
        if let error = Core.errorOf("set_sounds_on", ["on": on.state == .on]) {
            showAlert(error, on: view.window)
        }
        reload()
    }

    @objc private func choiceChanged() {
        guard let event = selected,
            let kind = choice.selectedItem?.representedObject as? String
        else { return }
        switch kind {
        case "choose": chooseFile(for: event)
        // The file that it plays already.
        case "file": break
        default: set(event, kind: kind, file: nil)
        }
    }

    /// Asks for a sound file for `event`. Until one is chosen, the event
    /// plays what it did.
    private func chooseFile(for event: SoundSetting) {
        guard let window = view.window else { return }
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.allowedContentTypes = [.audio]
        panel.message = L[.sound_pick_title]
        panel.prompt = L[.choose_folder_prompt]
        if let file = event.file {
            panel.directoryURL = URL(fileURLWithPath: file).deletingLastPathComponent()
        }
        panel.beginSheetModal(for: window) { [weak self] response in
            guard let self = self else { return }
            guard response == .OK, let url = panel.url else {
                self.showChoice()
                return
            }
            guard Sounds.canPlay(url.path) else {
                self.showChoice()
                showAlert(L[.sound_not_playable], on: window)
                return
            }
            self.set(event, kind: "file", file: url.path)
        }
    }

    /// Also while sounds are off: this is how one is tried.
    @objc private func playSelected() {
        guard let event = selected else { return }
        Core.perform("try_sound", ["event": event.event])
    }

    @objc private func resetAll() {
        if let error = Core.errorOf("reset_sounds", [:]) {
            showAlert(error, on: view.window)
        }
        reload()
        // To the list, which says what each event plays now.
        view.window?.makeFirstResponder(table)
    }

    func numberOfRows(in _: NSTableView) -> Int {
        events.count
    }

    func tableView(_ tableView: NSTableView, viewFor column: NSTableColumn?, row: Int) -> NSView? {
        guard let id = column?.identifier, events.indices.contains(row) else { return nil }
        let cell =
            tableView.makeView(withIdentifier: id, owner: self) as? NSTableCellView
            ?? makeTextCell(id)
        let event = events[row]
        cell.textField?.stringValue = id.rawValue == "event" ? event.name : event.shown
        cell.textField?.toolTip = id.rawValue == "sound" ? event.file : nil
        return cell
    }

    func tableViewSelectionDidChange(_: Notification) {
        showChoice()
    }
}
