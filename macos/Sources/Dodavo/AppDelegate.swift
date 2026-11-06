import AppKit
import UserNotifications

/// Called from any thread when Rust has something for the user interface.
private func wakeUp(_: UnsafeMutableRawPointer?) {
    DispatchQueue.main.async {
        (NSApp.delegate as? AppDelegate)?.takeMessages()
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate, NSMenuDelegate {
    private var started: Started?
    private var mainWindow: MainWindowController?
    private var transfersWindow: TransfersWindowController?
    private var settingsWindow: SettingsWindowController?
    private var licenseWindow: LicenseWindowController?
    private var updateWindow: UpdateWindowController?
    private var statusItem: StatusItem?
    private var notifier: Notifier?
    private var incoming: [UInt64: IncomingWindowController] = [:]
    private var rows: [UInt64: Row] = [:]
    /// Kept from throttling while Dodavo runs hidden, so that it can receive.
    private var running: NSObjectProtocol?
    /// Kept from going to sleep by itself while data moves.
    private var moving: NSObjectProtocol?
    private var sendingAgain = false
    /// What the status line says.
    private(set) var statusText = ""
    /// Until when a reopen comes from the notification that was just clicked.
    private var quietUntil = Date.distantPast
    /// Items waiting for the program to be ready (dropped on the Dock icon).
    private var pendingPaths: [String] = []

    var isReady: Bool { started != nil }

    // MARK: Starting

    func applicationWillFinishLaunching(_: Notification) {
        var profile: String?
        var hidden = LoginItem.launchedAtLogin
        let arguments = CommandLine.arguments
        var i = 1
        while i < arguments.count {
            switch arguments[i] {
            case "--profile" where i + 1 < arguments.count:
                profile = arguments[i + 1]
                i += 1
            case "--minimized":
                hidden = true
            default:
                break
            }
            i += 1
        }
        guard let started = Core.start(profile: profile, hidden: hidden, wake: wakeUp) else {
            fatal("The program could not start.")
            return
        }
        L.load()
        if let text = started.fatal {
            fatal(text)
            return
        }
        if started.alreadyRunning {
            activateRunningInstance()
            NSApp.terminate(nil)
            return
        }
        self.started = started
        applyAppearanceOverride()
        NSApp.mainMenu = buildMainMenu()
        NSApp.servicesMenu = servicesMenu
        NSApp.windowsMenu = windowMenu
        NSApp.helpMenu = helpMenu
        NSApp.servicesProvider = self
        notifier = Notifier(open: { [weak self] action in self?.perform(action) })
        running = ProcessInfo.processInfo.beginActivity(
            options: [
                .userInitiatedAllowingIdleSystemSleep,
                .automaticTerminationDisabled,
                .suddenTerminationDisabled,
            ],
            reason: "Receiving files")
    }

    func applicationDidFinishLaunching(_: Notification) {
        guard let started = started else { return }
        let main = MainWindowController(started: started, title: windowTitle)
        mainWindow = main
        if started.showInMenuBar {
            statusItem = StatusItem(delegate: self)
        }
        if !started.startHidden {
            main.showWindow(nil)
            NSApp.activate(ignoringOtherApps: true)
        }
        if let warning = started.warning {
            showAlert(warning, on: main.window)
        }
        takeMessages()
        #if DODAVO_UI_TEST
            openEverythingForTesting()
        #endif
        if CommandLine.arguments.contains("--updated") {
            notifier?.post(
                Notice(
                    title: L[.updated_balloon_title],
                    text: L.f(.updated_balloon, ["version": started.version])),
                action: .none)
        }
        if !started.startHidden {
            Relocation.offerIfNeeded(on: main.window)
        }
        let paths = pendingPaths
        pendingPaths = []
        if !paths.isEmpty {
            chooseRecipient(forSending: paths)
        }
    }

    private var windowTitle: String {
        if let profile = started?.profile { return "Dodavo [\(profile)]" }
        return "Dodavo"
    }

    /// `DODAVO_THEME=dark|light` overrides the appearance, for testing.
    private func applyAppearanceOverride() {
        switch ProcessInfo.processInfo.environment["DODAVO_THEME"] {
        case "dark": NSApp.appearance = NSAppearance(named: .darkAqua)
        case "light": NSApp.appearance = NSAppearance(named: .aqua)
        default: break
        }
    }

    private func fatal(_ text: String) {
        let alert = NSAlert()
        alert.alertStyle = .critical
        alert.messageText = text
        alert.addButton(withTitle: L[.close])
        NSApp.activate(ignoringOtherApps: true)
        alert.runModal()
        exit(1)
    }

    /// LaunchServices already reuses a running copy; this is for the one
    /// started by hand with `open -n`.
    private func activateRunningInstance() {
        let me = ProcessInfo.processInfo.processIdentifier
        for app in NSRunningApplication.runningApplications(
            withBundleIdentifier: Bundle.main.bundleIdentifier ?? "")
        where app.processIdentifier != me {
            app.activate(options: [])
        }
    }

    func applicationShouldTerminateAfterLastWindowClosed(_: NSApplication) -> Bool {
        false
    }

    func applicationSupportsSecureRestorableState(_: NSApplication) -> Bool {
        true
    }

    /// The Dock icon, and also a click on a notification, which has a place of its
    /// own to lead to (see `perform`), and must not end up here instead.
    func applicationShouldHandleReopen(_: NSApplication, hasVisibleWindows visible: Bool) -> Bool {
        if !visible, Date() >= quietUntil { showMainWindow(nil) }
        return true
    }

    // MARK: Messages from Rust

    func takeMessages() {
        Core.drain(handle)
    }

    private func handle(_ message: Message) {
        switch message {
        case .status(_, let text):
            statusText = text
            mainWindow?.setStatus(text)
        case .row(let row, let announcement):
            rows[row.id] = row
            mainWindow?.setRows(Array(rows.values))
            transfersWindow?.update(row)
            refreshCounts()
            let saidByNotification = !row.active && notifier?.showsEndOfTransfer == true
            if let announcement = announcement, NSApp.isActive, !saidByNotification {
                announce("\(row.item): \(announcement)")
            }
        case .rowGone(let id):
            rows[id] = nil
            mainWindow?.setRows(Array(rows.values))
            transfersWindow?.remove(id)
            refreshCounts()
        case .notice(let id, let notice, let path):
            notifier?.post(notice, action: .transfer(id, path: path))
        case .offer(let offer, let notice):
            let window = IncomingWindowController(offer: offer) { [weak self] in
                self?.incoming[offer.id] = nil
                self?.notifier?.forgetOffer(offer.id)
            }
            incoming[offer.id] = window
            window.present()
            NSApp.requestUserAttention(.informationalRequest)
            notifier?.post(notice, action: .offer(offer.id))
        case .offerGone(let id, let notice):
            incoming[id]?.closeWithoutAnswer()
            incoming[id] = nil
            notifier?.post(notice, action: .none)
        case .busy(let busy):
            setMovingData(busy)
        case .error(let text):
            showAlert(text, on: mainWindow?.window)
        case .fatal(let text):
            fatal(text)
        case .updateAvailable(let headline, let notes, let manual):
            showUpdate(headline: headline, notes: notes, manual: manual)
        case .updateAnswer(let text):
            mainWindow?.setCheckingStatus(false)
            showAlert(text, on: mainWindow?.window?.isVisible == true ? mainWindow?.window : nil)
        case .updateProgress(let text, let fraction):
            updateWindow?.setProgress(text, fraction: fraction)
        case .updateReady(let path):
            updateWindow?.install(from: path)
        case .updateFailed(let text):
            updateWindow?.fail(text)
        }
    }

    private func showUpdate(headline: String, notes: String, manual: Bool) {
        mainWindow?.setCheckingStatus(false)
        if updateWindow == nil {
            updateWindow = UpdateWindowController(headline: headline, notes: notes) { [weak self] in
                self?.updateWindow = nil
            }
        }
        if manual {
            updateWindow?.showWindow(nil)
            updateWindow?.window?.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
        } else {
            // A check that nobody asked for takes no focus away.
            updateWindow?.window?.orderFrontRegardless()
            notifier?.post(
                Notice(title: L[.update_title], text: headline), action: .none)
        }
    }

    private func setMovingData(_ busy: Bool) {
        if busy, moving == nil {
            moving = ProcessInfo.processInfo.beginActivity(
                options: .idleSystemSleepDisabled, reason: "Transferring files")
        } else if !busy, let activity = moving {
            ProcessInfo.processInfo.endActivity(activity)
            moving = nil
        }
    }

    var runningRows: [Row] {
        rows.values.filter { $0.active }.sorted { ($0.peer, $0.item) < ($1.peer, $1.item) }
    }

    private func refreshCounts() {
        let count = runningRows.count
        NSApp.dockTile.badgeLabel = count > 0 ? "\(count)" : nil
        statusItem?.update(running: runningRows)
    }

    // MARK: What notifications lead to

    private func perform(_ action: Notifier.Action) {
        quietUntil = Date().addingTimeInterval(1)
        switch action {
        case .none:
            showMainWindow(nil)
        case .transfer(let id, _):
            showTransfers(nil)
            transfersWindow?.reveal(id)
        case .offer(let id):
            incoming[id]?.bringToFront()
        case .open(let path):
            NSWorkspace.shared.open(URL(fileURLWithPath: path))
        case .reveal(let path):
            NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: path)])
            // It does not come to the front when asked by an app that is not there
            // itself, as with the action of a notification.
            let configuration = NSWorkspace.OpenConfiguration()
            configuration.activates = true
            NSWorkspace.shared.openApplication(
                at: URL(fileURLWithPath: "/System/Library/CoreServices/Finder.app"),
                configuration: configuration)
        }
    }

    // MARK: Commands

    @objc func showMainWindow(_: Any?) {
        mainWindow?.showWindow(nil)
        mainWindow?.window?.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    @objc func showTransfers(_: Any?) {
        if transfersWindow == nil {
            transfersWindow = TransfersWindowController(rows: Array(rows.values))
        }
        transfersWindow?.showWindow(nil)
        transfersWindow?.window?.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    @objc func showTransferFromMenu(_ sender: NSMenuItem) {
        showTransfers(nil)
        transfersWindow?.reveal(UInt64(sender.tag))
    }

    @objc func showSettings(_: Any?) {
        if settingsWindow == nil, let started = started {
            settingsWindow = SettingsWindowController(started: started) { [weak self] in
                self?.applySettings()
            }
        }
        settingsWindow?.showWindow(nil)
        settingsWindow?.window?.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    private func applySettings() {
        guard let settings = Core.settings() else { return }
        if settings.showInMenuBar {
            if statusItem == nil {
                statusItem = StatusItem(delegate: self)
                statusItem?.update(running: runningRows)
            }
        } else {
            statusItem = nil
        }
    }

    @objc func showAbout(_: Any?) {
        guard let started = started else { return }
        let text = NSMutableAttributedString(
            string: "\(L[.about_text])\n\n\(L[.about_notice])\n\n",
            attributes: [
                .font: NSFont.systemFont(ofSize: NSFont.smallSystemFontSize),
                .foregroundColor: NSColor.labelColor,
            ])
        text.append(
            NSAttributedString(
                string: started.website,
                attributes: [
                    .font: NSFont.systemFont(ofSize: NSFont.smallSystemFontSize),
                    .link: started.website,
                ]))
        NSApp.orderFrontStandardAboutPanel(options: [
            .applicationName: "Dodavo",
            .applicationVersion: started.version,
            .version: "",
            .credits: text,
            NSApplication.AboutPanelOptionKey(rawValue: "Copyright"):
                "Copyright © \(started.copyright) Hrvoje Katić",
        ])
        NSApp.activate(ignoringOtherApps: true)
    }

    @objc func showLicense(_: Any?) {
        if licenseWindow == nil {
            licenseWindow = LicenseWindowController()
        }
        licenseWindow?.showWindow(nil)
        licenseWindow?.window?.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    @objc func openHelp(_: Any?) {
        let language = started?.language ?? "en"
        guard
            let url = Bundle.main.url(
                forResource: language, withExtension: "html", subdirectory: "help")
                ?? Bundle.main.url(forResource: "en", withExtension: "html", subdirectory: "help")
        else {
            showAlert(L.f(.help_failed, ["error": ""]), on: mainWindow?.window)
            return
        }
        NSWorkspace.shared.open(url)
    }

    @objc func openDonate(_: Any?) {
        if let text = started?.donateUrl, let url = URL(string: text) {
            NSWorkspace.shared.open(url)
        }
    }

    @objc func copyMyContactID(_: Any?) {
        guard let id = started?.myId else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(id, forType: .string)
        announce(L[.id_copied])
        mainWindow?.flashStatus(L[.id_copied])
    }

    // MARK: Sending from elsewhere

    /// Files dropped on the Dock icon, or opened with Dodavo.
    func application(_: NSApplication, open urls: [URL]) {
        let paths = urls.filter { $0.isFileURL }.map { $0.path }
        if isReady, mainWindow != nil {
            chooseRecipient(forSending: paths)
        } else {
            pendingPaths += paths
        }
    }

    /// The Finder's service "Send with Dodavo" (see `NSServices` in Info.plist).
    @objc func sendWithDodavo(
        _ pasteboard: NSPasteboard, userData _: String,
        error _: AutoreleasingUnsafeMutablePointer<NSString>
    ) {
        let urls = pasteboard.readObjects(
            forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true])
        let paths = (urls as? [URL] ?? []).map { $0.path }
        NSApp.activate(ignoringOtherApps: true)
        chooseRecipient(forSending: paths)
    }

    private func chooseRecipient(forSending paths: [String]) {
        guard !paths.isEmpty else { return }
        showMainWindow(nil)
        mainWindow?.chooseRecipient(forSending: paths)
    }

    // MARK: Quitting

    private var alreadyAsked = false

    func applicationShouldTerminate(_: NSApplication) -> NSApplication.TerminateReply {
        guard isReady, !alreadyAsked else { return .terminateNow }
        // Logging out, restarting or shutting down is never held up: the
        // transfers stop, and go on at the next start.
        if let event = NSAppleEventManager.shared().currentAppleEvent,
            let reason = event.attributeDescriptor(forKeyword: kAEQuitReason)?.enumCodeValue,
            reason == kAELogOut || reason == kAEReallyLogOut || reason == kAEShowRestartDialog
                || reason == kAERestart || reason == kAEShowShutdownDialog || reason == kAEShutDown
        {
            return .terminateNow
        }
        return mayQuit() ? .terminateNow : .terminateCancel
    }

    /// With transfers running, asks what becomes of them. Whether to go on and quit.
    func mayQuit() -> Bool {
        guard !runningRows.isEmpty else { return true }
        let alert = NSAlert()
        alert.messageText = L.plural(.quit_instruction, runningRows.count)
        alert.informativeText = L[.quit_text]
        alert.addButton(withTitle: L[.quit_keep])
        alert.addButton(withTitle: L[.quit_cancel_transfers])
        alert.addButton(withTitle: L[.dont_quit])
        alert.buttons[2].keyEquivalent = "\u{1b}"
        NSApp.activate(ignoringOtherApps: true)
        switch alert.runModal() {
        case .alertFirstButtonReturn:
            return true
        case .alertSecondButtonReturn:
            Core.perform("cancel_all")
            return true
        default:
            return false
        }
    }

    /// For what asked with `mayQuit` already.
    func quitWithoutAsking() {
        alreadyAsked = true
        NSApp.terminate(nil)
    }

    func applicationWillTerminate(_: Notification) {
        if isReady {
            Core.perform("shutdown")
        }
    }

    // MARK: The menus

    private let servicesMenu = NSMenu()
    private let windowMenu = NSMenu()
    private let helpMenu = NSMenu()
    private let transfersMenu = NSMenu()

    private func buildMainMenu() -> NSMenu {
        let main = NSMenu()

        // The application menu.
        let app = NSMenu()
        add(app, L[.menu_about], #selector(showAbout), "")
        app.addItem(.separator())
        add(app, L[.menu_check_updates], #selector(checkForUpdates), "")
        add(app, L.settingsMenuItem, #selector(showSettings), ",")
        app.addItem(.separator())
        servicesMenu.title = L[.menu_services]
        let services = NSMenuItem(title: L[.menu_services], action: nil, keyEquivalent: "")
        services.submenu = servicesMenu
        app.addItem(services)
        app.addItem(.separator())
        add(app, L[.menu_hide], #selector(NSApplication.hide(_:)), "h", target: NSApp)
        let hideOthers = add(
            app, L[.menu_hide_others], #selector(NSApplication.hideOtherApplications(_:)), "h",
            target: NSApp)
        hideOthers.keyEquivalentModifierMask = [.command, .option]
        add(
            app, L[.menu_show_all], #selector(NSApplication.unhideAllApplications(_:)), "",
            target: NSApp)
        app.addItem(.separator())
        add(app, L[.menu_quit], #selector(NSApplication.terminate(_:)), "q", target: NSApp)
        addSubmenu(main, "Dodavo", app)

        // File.
        let file = NSMenu()
        file.autoenablesItems = true
        add(file, L[.menu_send], #selector(MainWindowController.send(_:)), "o", target: nil)
        file.addItem(.separator())
        add(
            file, L[.menu_add_recipient], #selector(MainWindowController.addRecipient(_:)), "n",
            target: nil)
        add(
            file, L[.menu_edit_recipient], #selector(MainWindowController.editRecipient(_:)), "i",
            target: nil)
        let remove = add(
            file, L[.menu_remove_recipient], #selector(MainWindowController.removeRecipient(_:)),
            "\u{8}", target: nil)
        remove.keyEquivalentModifierMask = [.command]
        add(
            file, L[.menu_block_recipient], #selector(MainWindowController.blockRecipient(_:)), "",
            target: nil)
        file.addItem(.separator())
        let copyID = add(file, L[.menu_copy_id], #selector(copyMyContactID), "c")
        copyID.keyEquivalentModifierMask = [.command, .shift]
        file.addItem(.separator())
        add(file, L[.menu_close], #selector(NSWindow.performClose(_:)), "w", target: nil)
        addSubmenu(main, L[.menu_file], file)

        // Edit, which text fields need for ⌘C and ⌘V to work at all.
        let edit = NSMenu()
        add(edit, L[.menu_undo], Selector(("undo:")), "z", target: nil)
        let redo = add(edit, L[.menu_redo], Selector(("redo:")), "z", target: nil)
        redo.keyEquivalentModifierMask = [.command, .shift]
        edit.addItem(.separator())
        add(edit, L[.menu_cut], #selector(NSText.cut(_:)), "x", target: nil)
        add(edit, L[.menu_copy], #selector(NSText.copy(_:)), "c", target: nil)
        add(edit, L[.menu_paste], #selector(NSText.paste(_:)), "v", target: nil)
        add(edit, L[.menu_delete], #selector(NSText.delete(_:)), "", target: nil)
        add(edit, L[.menu_select_all], #selector(NSText.selectAll(_:)), "a", target: nil)
        addSubmenu(main, L[.menu_edit], edit)

        // Transfers, filled in when it opens.
        transfersMenu.delegate = self
        addSubmenu(main, L[.menu_transfers], transfersMenu)

        // Window.
        add(windowMenu, L[.menu_minimize], #selector(NSWindow.performMiniaturize(_:)), "m", target: nil)
        add(windowMenu, L[.menu_zoom], #selector(NSWindow.performZoom(_:)), "", target: nil)
        windowMenu.addItem(.separator())
        add(
            windowMenu, L[.menu_bring_all_to_front], #selector(NSApplication.arrangeInFront(_:)),
            "", target: NSApp)
        addSubmenu(main, L[.menu_window], windowMenu)

        // Help.
        add(helpMenu, L[.menu_dodavo_help], #selector(openHelp), "?")
        helpMenu.addItem(.separator())
        add(helpMenu, L[.menu_license], #selector(showLicense), "")
        add(helpMenu, L[.menu_donate], #selector(openDonate), "")
        addSubmenu(main, L[.menu_help], helpMenu)
        return main
    }

    @discardableResult
    private func add(
        _ menu: NSMenu, _ title: String, _ action: Selector, _ key: String, target: AnyObject? = nil
    ) -> NSMenuItem {
        let item = menu.addItem(withTitle: title, action: action, keyEquivalent: key)
        // By default the application's own commands; `nil` for the responder chain.
        item.target = target ?? (responds(to: action) ? self : nil)
        return item
    }

    private func addSubmenu(_ main: NSMenu, _ title: String, _ submenu: NSMenu) {
        submenu.title = title
        let item = NSMenuItem(title: title, action: nil, keyEquivalent: "")
        item.submenu = submenu
        main.addItem(item)
    }

    /// Fills in a Transfers menu, here and in the menu bar.
    func menuNeedsUpdate(_ menu: NSMenu) {
        menu.removeAllItems()
        fillTransfers(menu)
    }

    func fillTransfers(_ menu: NSMenu) {
        let running = runningRows.prefix(50)
        if running.isEmpty {
            let none = NSMenuItem(title: L[.menu_no_transfers], action: nil, keyEquivalent: "")
            none.isEnabled = false
            menu.addItem(none)
        }
        for row in running {
            let item = NSMenuItem(
                title: "\(row.peer): \(row.state)",
                action: #selector(showTransferFromMenu(_:)), keyEquivalent: "")
            item.tag = Int(row.id)
            item.target = self
            menu.addItem(item)
        }
        menu.addItem(.separator())
        let all = NSMenuItem(
            title: L[.menu_show_transfers], action: #selector(showTransfers(_:)), keyEquivalent: "t")
        all.target = self
        menu.addItem(all)
    }

    /// The list changed from elsewhere (see `IncomingWindowController`).
    func recipientsChanged() {
        mainWindow?.recipientsChanged()
    }

    /// A sender was blocked (see `Core.block`): the windows of the offers of
    /// theirs that were declined close, and the lists show it.
    func senderBlocked(declined: [UInt64], error: String?) {
        for id in declined {
            incoming[id]?.closeWithoutAnswer()
            incoming[id] = nil
            notifier?.forgetOffer(id)
        }
        mainWindow?.recipientsChanged()
        settingsWindow?.blockedChanged()
        if let error = error { showAlert(error, on: mainWindow?.window) }
    }

    @objc func checkForUpdates(_: Any?) {
        mainWindow?.setCheckingStatus(true)
        Core.perform("check_for_updates")
    }
}

#if DODAVO_UI_TEST
    // A build for looking at the windows without a peer (see macos/tests/ui-preview.sh):
    // opens them all with made-up transfers, and reports controls without a name
    // from inside the program, which works with the screen locked as well.
    extension AppDelegate {
        private func sampleRow(
            _ id: UInt64, _ item: String, sending: Bool, status: String, fraction: Double?,
            active: Bool = true, succeeded: Bool = false, path: String? = nil, paused: Bool = false,
            amount: String = "", speed: String = "", remaining: String = "", detail: String = ""
        ) -> Row {
            let json: [String: Any?] = [
                "id": id, "peer_id": "abc", "peer": "Ana", "sending": sending, "item": item,
                "active": active, "succeeded": succeeded, "status": status, "state": status,
                "fraction": fraction, "amount": amount, "speed": speed, "remaining": remaining,
                "detail": detail, "can_pause": active, "paused": paused, "can_cancel": active,
                "path": path, "moving_data": active,
            ]
            let data = try! JSONSerialization.data(withJSONObject: json.compactMapValues { $0 })
            let decoder = JSONDecoder()
            decoder.keyDecodingStrategy = .convertFromSnakeCase
            return try! decoder.decode(Row.self, from: data)
        }

        func openEverythingForTesting() {
            let samples = [
                sampleRow(
                    1, "Report 2026.pdf", sending: true, status: "Sending…", fraction: 0.45,
                    amount: "45 MB of 100 MB (45%)", speed: "12 MB/s", remaining: "5 seconds left",
                    detail: "Connected directly"),
                sampleRow(
                    2, "Holiday photos", sending: false, status: "Paused", fraction: 0.2,
                    paused: true, amount: "200 MB of 1 GB (20%)"),
                sampleRow(3, "Kuća.jpg", sending: true, status: "Connecting to Ana…", fraction: nil),
                sampleRow(
                    4, "Račun.pdf", sending: false, status: "The file has been saved.", fraction: 1,
                    active: false, succeeded: true, path: NSHomeDirectory() + "/Downloads/Račun.pdf",
                    amount: "1 MB of 1 MB (100%)", detail: NSHomeDirectory() + "/Downloads/Račun.pdf"),
                sampleRow(
                    5, "Too big.mov", sending: true, status: "The transfer failed.", fraction: 0.1,
                    active: false, detail: "There is not enough free disk space."),
            ]
            for row in samples { handle(.row(row, announce: nil)) }
            showTransfers(nil)
            showSettings(nil)
            showLicense(nil)
            showAbout(nil)
            updateWindow = UpdateWindowController(
                headline: "Dodavo 1.1.0 is available (you have 1.0.0).",
                notes: "What is new:\n\n- The Mac version.\n- Fixes.", closed: {})
            updateWindow?.showWindow(nil)
            let offer = OfferInfo(
                id: 9, fromId: "abcdef", fromName: "Marko", known: false,
                headline: "Marko wants to send you a folder",
                description: "“Holiday photos”  (12 files, 2 GB)",
                unknownWarning: "This sender is not in your recipient list (Contact ID abcdef).",
                addLabel: "Add “Marko” to my recipients", dir: NSHomeDirectory() + "/Downloads",
                space: SpaceInfo(
                    text: "There is enough free space: 2 GB needed, 90 GB available on “Macintosh HD”.",
                    fits: true, enough: true))
            let incomingWindow = IncomingWindowController(offer: offer) {}
            incoming[9] = incomingWindow
            incomingWindow.present()
            mainWindow?.setRows(samples)
            DispatchQueue.main.asyncAfter(deadline: .now() + 1) { self.auditForTesting() }
        }

        /// Controls that VoiceOver would have no name for.
        private func auditForTesting() {
            var report = ""
            var controls = 0
            var window: NSWindow?
            func look(_ view: NSView, _ path: String) {
                let kind = String(describing: type(of: view))
                let here = "\(path)/\(kind)"
                var isControl =
                    view is NSButton || view is NSProgressIndicator || view is NSTableView
                if let field = view as? NSTextField, field.isEditable { isControl = true }
                // A field being edited, and the About panel, are the system's.
                if let text = view as? NSTextView, !text.isFieldEditor, window?.title != "" {
                    isControl = true
                }
                if isControl {
                    controls += 1
                    let label = view.accessibilityLabel() ?? ""
                    let title = (view as? NSButton)?.title ?? view.accessibilityTitle() ?? ""
                    let linked = view.accessibilityTitleUIElement() != nil
                    if label.isEmpty && title.isEmpty && !linked {
                        report += "NO NAME: \(here)\n"
                    }
                }
                for child in view.subviews { look(child, here) }
            }
            for each in NSApp.windows where each.isVisible {
                window = each
                report += "Window: \(each.title)\n"
                if let content = each.contentView { look(content, "") }
            }
            report += "\(controls) controls looked at\n"
            let file = ProcessInfo.processInfo.environment["DODAVO_AUDIT_FILE"] ?? "/tmp/dodavo-audit.txt"
            try? report.write(toFile: file, atomically: true, encoding: .utf8)
        }
    }
#endif
