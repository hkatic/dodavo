import AppKit

/// A newer version: its release notes, and installing it. The download and its
/// checks are in Rust; what comes after them here is what only a Mac needs.
final class UpdateWindowController: NSWindowController, NSWindowDelegate {
    private let closed: () -> Void
    private let statusLine = makeLabel()
    private let bar = NSProgressIndicator()
    private let notesView = NSTextView()
    private let installButton: NSButton
    private let laterButton: NSButton
    private let cancelButton: NSButton
    private var working = false
    private var installing = false

    init(headline: String, notes: String, closed: @escaping () -> Void) {
        self.closed = closed
        installButton = NSButton()
        laterButton = NSButton()
        cancelButton = NSButton()
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 520, height: 400),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered, defer: false)
        window.title = L[.update_title]
        window.isReleasedWhenClosed = false
        super.init(window: window)
        window.delegate = self
        build(window, headline: headline, notes: notes)
        window.center()
    }

    required init?(coder _: NSCoder) { nil }

    private func build(_ window: NSWindow, headline: String, notes: String) {
        let heading = makeLabel(headline, bold: true)
        let notesLabel = NSTextField(labelWithString: L[.release_notes_label])
        notesView.isEditable = false
        notesView.string = notes
        notesView.font = .systemFont(ofSize: NSFont.systemFontSize)
        notesView.setAccessibilityLabel(L[.release_notes_label])
        notesView.isVerticallyResizable = true
        notesView.autoresizingMask = .width
        let scroll = NSScrollView()
        scroll.documentView = notesView
        scroll.hasVerticalScroller = true
        scroll.borderType = .bezelBorder
        scroll.translatesAutoresizingMaskIntoConstraints = false
        scroll.heightAnchor.constraint(greaterThanOrEqualToConstant: 180).isActive = true

        statusLine.isHidden = true
        bar.isHidden = true
        bar.minValue = 0
        bar.maxValue = 1
        bar.isIndeterminate = false
        bar.style = .bar
        bar.setAccessibilityLabel(L[.update_title])

        for (button, title, action) in [
            (installButton, L[.install_update], #selector(installClicked)),
            (laterButton, L[.remind_later], #selector(remindLater)),
            (cancelButton, L[.cancel_download], #selector(cancelDownload)),
        ] as [(NSButton, String, Selector)] {
            button.title = title
            button.bezelStyle = .rounded
            button.target = self
            button.action = action
        }
        installButton.keyEquivalent = "\r"
        laterButton.keyEquivalent = "\u{1b}"
        cancelButton.keyEquivalent = "\u{1b}"
        cancelButton.isHidden = true
        let buttons = makeButtonRow([cancelButton, laterButton, installButton])
        let column = makeColumn([heading, notesLabel, scroll, statusLine, bar, buttons], spacing: 10)
        fill(window, with: column)
        NSLayoutConstraint.activate([
            scroll.widthAnchor.constraint(equalTo: column.widthAnchor),
            bar.widthAnchor.constraint(equalTo: column.widthAnchor),
            buttons.widthAnchor.constraint(equalTo: column.widthAnchor),
            statusLine.widthAnchor.constraint(equalTo: column.widthAnchor),
        ])
        window.initialFirstResponder = notesView
    }

    // MARK: Downloading

    @objc private func installClicked() {
        guard Relocation.canReplaceItself else {
            fail(L[.update_move_first])
            return
        }
        working = true
        statusLine.stringValue = L[.update_verifying]
        statusLine.isHidden = false
        bar.isHidden = false
        bar.doubleValue = 0
        window?.makeFirstResponder(cancelButton)
        installButton.isHidden = true
        laterButton.isHidden = true
        cancelButton.isHidden = false
        Core.perform("download_update")
    }

    func setProgress(_ text: String, fraction: Double?) {
        let changed = statusLine.stringValue != text && fraction == nil
        statusLine.stringValue = text
        if let fraction = fraction {
            bar.isIndeterminate = false
            bar.doubleValue = fraction
        } else {
            bar.isIndeterminate = true
            bar.startAnimation(nil)
        }
        bar.setAccessibilityValue(text)
        if changed { announce(text) }
    }

    func fail(_ text: String) {
        working = false
        installing = false
        statusLine.stringValue = text
        statusLine.isHidden = false
        bar.isHidden = true
        bar.stopAnimation(nil)
        installButton.isHidden = false
        laterButton.isHidden = false
        window?.makeFirstResponder(installButton)
        cancelButton.isHidden = true
        cancelButton.isEnabled = true
        announce(text)
    }

    @objc private func cancelDownload() {
        Core.perform("cancel_update")
        fail(L[.cancelled_text])
    }

    @objc private func remindLater() {
        Core.perform("remind_later")
        window?.performClose(nil)
    }

    func windowShouldClose(_: NSWindow) -> Bool {
        if installing { return false }
        if working {
            Core.perform("cancel_update")
        }
        return true
    }

    func windowWillClose(_: Notification) {
        Core.perform("update_closed")
        closed()
    }

    // MARK: Installing

    /// The disk image is here, and its size and signature are checked. Asks
    /// about running transfers as quitting does, then replaces the app.
    func install(from dmg: String) {
        installing = true
        cancelButton.isEnabled = false
        statusLine.stringValue = L[.update_installing]
        bar.isIndeterminate = true
        bar.startAnimation(nil)
        announce(L[.update_installing])
        let running = Bundle.main.bundleURL
        DispatchQueue.global(qos: .userInitiated).async {
            let result = Result { try Installer.replace(running, withAppIn: dmg) }
            DispatchQueue.main.async {
                switch result {
                case .success(.replaced):
                    Installer.relaunch(running)
                    NSApp.terminate(nil)
                case .success(.leftToTheUser):
                    self.fail(L[.update_manual])
                case .failure(let error):
                    self.fail(L.f(.update_err_install, ["detail": "\(error)"]))
                }
            }
        }
    }
}

/// What comes after the download.
enum Installer {
    enum Outcome {
        case replaced
        case leftToTheUser
    }

    struct Failure: Error, CustomStringConvertible {
        let description: String
    }

    /// Runs a program, and gives what it printed; fails if it fails.
    @discardableResult
    private static func run(_ tool: String, _ arguments: [String]) throws -> String {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: tool)
        process.arguments = arguments
        let output = Pipe()
        process.standardOutput = output
        process.standardError = output
        try process.run()
        let data = output.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        let text = String(data: data, encoding: .utf8) ?? ""
        guard process.terminationStatus == 0 else {
            throw Failure(description: text.trimmingCharacters(in: .whitespacesAndNewlines))
        }
        return text
    }

    /// The team that signed an app, as `codesign` says.
    private static func team(of app: URL) -> String? {
        guard let text = try? run("/usr/bin/codesign", ["-dv", "--verbose=2", app.path]) else {
            return nil
        }
        for line in text.split(separator: "\n") where line.hasPrefix("TeamIdentifier=") {
            let team = String(line.dropFirst("TeamIdentifier=".count))
            return team == "not set" ? nil : team
        }
        return nil
    }

    /// Replaces the app at `running` with the one on the disk image, in one
    /// step, or leaves that to the user where this user may not.
    static func replace(_ running: URL, withAppIn dmg: String) throws -> Outcome {
        let parent = running.deletingLastPathComponent()
        guard FileManager.default.isWritableFile(atPath: parent.path) else {
            NSWorkspace.shared.open(URL(fileURLWithPath: dmg))
            return .leftToTheUser
        }
        let mount = FileManager.default.temporaryDirectory
            .appendingPathComponent("dodavo-update-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: mount, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: mount) }
        do {
            try run(
                "/usr/bin/hdiutil",
                ["attach", dmg, "-nobrowse", "-readonly", "-noautoopen", "-mountpoint", mount.path])
        } catch {
            throw Failure(description: L.f(.update_err_mount, ["detail": "\(error)"]))
        }
        defer { _ = try? run("/usr/bin/hdiutil", ["detach", mount.path, "-force"]) }
        let source = mount.appendingPathComponent("Dodavo.app")

        // Whatever the manifest says, the app must be whole and from the same
        // team as the one that is running.
        do {
            try run("/usr/bin/codesign", ["--verify", "--deep", "--strict", source.path])
        } catch {
            throw Failure(description: L[.update_err_not_signed])
        }
        if let mine = team(of: running), team(of: source) != mine {
            throw Failure(description: L[.update_err_not_signed])
        }

        // Beside the old one, on its volume, so that the swap is one step: the
        // running program goes on from the old files until it quits.
        let fresh = parent.appendingPathComponent(".Dodavo-update-\(UUID().uuidString).app")
        try run("/usr/bin/ditto", [source.path, fresh.path])
        defer { try? FileManager.default.removeItem(at: fresh) }
        let swapped = fresh.path.withCString { from in
            running.path.withCString { to in
                renamex_np(from, to, UInt32(RENAME_SWAP))
            }
        }
        guard swapped == 0 else {
            throw Failure(description: String(cString: strerror(errno)))
        }
        return .replaced
    }

    /// Opens the new app once this one has gone: it holds the lock of the
    /// profile until then, and a second copy would quit.
    static func relaunch(_ app: URL) {
        let script = """
            while kill -0 \(ProcessInfo.processInfo.processIdentifier) 2>/dev/null; do sleep 0.2; done
            exec /usr/bin/open "$0" --args --updated
            """
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/bin/sh")
        process.arguments = ["-c", script, app.path]
        try? process.run()
    }
}

/// Where the app is. Run from Downloads, or from the disk image, macOS gives it
/// a place of its own that nothing can replace ("App Translocation").
enum Relocation {
    private static let declinedKey = "MoveToApplicationsDeclined"

    private static var inApplications: Bool {
        let path = Bundle.main.bundlePath
        let home = NSHomeDirectory() + "/Applications/"
        return path.hasPrefix("/Applications/") || path.hasPrefix(home)
    }

    static var isTranslocated: Bool {
        Bundle.main.bundlePath.contains("/AppTranslocation/")
    }

    private static var onDiskImage: Bool {
        Bundle.main.bundlePath.hasPrefix("/Volumes/")
    }

    /// Whether an update can put itself where this copy is.
    static var canReplaceItself: Bool {
        !isTranslocated && !onDiskImage
    }

    /// The first time from elsewhere, offers to move to the Applications folder.
    /// Also saves those who use VoiceOver a drag in the Finder.
    static func offerIfNeeded(on window: NSWindow?) {
        guard !inApplications, isTranslocated || onDiskImage || isInDownloads,
            !UserDefaults.standard.bool(forKey: declinedKey), let window = window
        else { return }
        let alert = NSAlert()
        alert.messageText = L[.move_title]
        alert.informativeText = L[.move_text]
        alert.addButton(withTitle: L[.move_button])
        alert.addButton(withTitle: L[.move_later])
        alert.buttons[1].keyEquivalent = "\u{1b}"
        alert.beginSheetModal(for: window) { response in
            guard response == .alertFirstButtonReturn else {
                UserDefaults.standard.set(true, forKey: declinedKey)
                return
            }
            do {
                let moved = try moveToApplications()
                Installer.relaunch(moved)
                NSApp.terminate(nil)
            } catch {
                showAlert(L.f(.move_failed, ["error": error.localizedDescription]), on: window)
            }
        }
    }

    private static var isInDownloads: Bool {
        Bundle.main.bundlePath.hasPrefix(NSHomeDirectory() + "/Downloads/")
    }

    private static func moveToApplications() throws -> URL {
        let manager = FileManager.default
        var folder = URL(fileURLWithPath: "/Applications")
        if !manager.isWritableFile(atPath: folder.path) {
            folder = URL(fileURLWithPath: NSHomeDirectory() + "/Applications")
            try manager.createDirectory(at: folder, withIntermediateDirectories: true)
        }
        let target = folder.appendingPathComponent("Dodavo.app")
        if manager.fileExists(atPath: target.path) {
            try manager.trashItem(at: target, resultingItemURL: nil)
        }
        try manager.copyItem(at: Bundle.main.bundleURL, to: target)
        // Copied, as a translocated or mounted copy cannot go: this one goes
        // where it can, and from the disk image it stays.
        if isInDownloads, !isTranslocated {
            try? manager.trashItem(at: Bundle.main.bundleURL, resultingItemURL: nil)
        }
        return target
    }
}
