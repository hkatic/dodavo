import AppKit

/// The menu bar extra: what the notification area icon is on Windows. Any click
/// opens its menu, as menu bar extras do.
final class StatusItem: NSObject, NSMenuDelegate {
    private let item: NSStatusItem
    private unowned let app: AppDelegate
    private let menu = NSMenu()

    init(delegate app: AppDelegate) {
        self.app = app
        item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        super.init()
        if let button = item.button {
            // A template, which follows the menu bar's appearance.
            let image = NSImage(
                systemSymbolName: "arrow.up.arrow.down", accessibilityDescription: "Dodavo")
            image?.isTemplate = true
            button.image = image
            button.imagePosition = .imageLeading
            // Not the number that shows beside the image, which would come instead.
            button.setAccessibilityTitle("Dodavo")
        }
        menu.delegate = self
        item.menu = menu
        update(running: [])
    }

    deinit {
        NSStatusBar.system.removeStatusItem(item)
    }

    /// The number of running transfers, and for VoiceOver and the pointer, what
    /// they are doing.
    func update(running: [Row]) {
        guard let button = item.button else { return }
        button.title = running.isEmpty ? "" : "\(running.count)"
        let summary: String
        switch running.count {
        case 0: summary = ""
        case 1: summary = running[0].state
        default: summary = L.plural(.transfers_in_progress, running.count)
        }
        button.toolTip = summary.isEmpty ? "Dodavo" : "Dodavo – \(summary)"
        // A tooltip is also what VoiceOver gives as help, after the name and the
        // value that say the same. Without a value it does not say the name either.
        button.setAccessibilityHelp("")
        button.setAccessibilityValue(summary.isEmpty ? L[.menu_no_transfers] : summary)
    }

    func menuNeedsUpdate(_ menu: NSMenu) {
        menu.removeAllItems()
        let open = NSMenuItem(
            title: L[.menu_open_dodavo], action: #selector(AppDelegate.showMainWindow(_:)),
            keyEquivalent: "")
        open.target = app
        menu.addItem(open)
        menu.addItem(.separator())
        app.fillTransfers(menu)
        menu.addItem(.separator())
        let copy = NSMenuItem(
            title: L[.menu_copy_id], action: #selector(AppDelegate.copyMyContactID(_:)),
            keyEquivalent: "")
        copy.target = app
        menu.addItem(copy)
        menu.addItem(.separator())
        let quit = NSMenuItem(
            title: L[.menu_quit], action: #selector(NSApplication.terminate(_:)), keyEquivalent: "")
        quit.target = NSApp
        menu.addItem(quit)
    }
}
