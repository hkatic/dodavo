import AppKit
import UserNotifications

/// What the balloons are on Windows: notifications, each with its own action.
final class Notifier: NSObject, UNUserNotificationCenterDelegate {
    enum Action {
        case none
        case transfer(UInt64, path: String?)
        case offer(UInt64)
        case open(String)
        case reveal(String)
    }

    private let open: (Action) -> Void
    /// Notifications need an app bundle; a program run from the build folder
    /// has none, and the framework would abort it.
    private let center: UNUserNotificationCenter? =
        Bundle.main.bundleIdentifier == nil ? nil : .current()
    private var asked = false
    /// Whether macOS shows them, as far as is known.
    private var allowed = false

    init(open: @escaping (Action) -> Void) {
        self.open = open
        super.init()
        center?.delegate = self
        // Not `.foreground`: what comes to the front is the app that opens the item,
        // or the Finder, and not Dodavo.
        let openItem = UNNotificationAction(identifier: "open", title: L[.open], options: [])
        let reveal = UNNotificationAction(
            identifier: "reveal", title: L[.show_in_finder], options: [])
        center?.setNotificationCategories([
            UNNotificationCategory(
                identifier: "saved", actions: [openItem, reveal], intentIdentifiers: [],
                options: [])
        ])
        refreshAllowed()
    }

    private func refreshAllowed() {
        center?.getNotificationSettings { settings in
            let allowed = settings.authorizationStatus == .authorized
            DispatchQueue.main.async { self.allowed = allowed }
        }
    }

    /// Whether the end of a transfer comes as a notification now. VoiceOver reads
    /// that one, so nothing else needs to say it. In the Transfers window the row
    /// says it (see `willPresent`).
    var showsEndOfTransfer: Bool {
        allowed && !transfersWindowIsInFront
    }

    private var transfersWindowIsInFront: Bool {
        NSApp.isActive && NSApp.keyWindow?.identifier?.rawValue == "transfers"
    }

    func post(_ notice: Notice, action: Action) {
        guard let center = center else {
            if NSApp.isActive { announce("\(notice.title): \(notice.text)") }
            return
        }
        if !asked {
            asked = true
            center.requestAuthorization(options: [.alert, .sound]) { _, _ in }
        }
        // Turned off in System Settings: the Dock asks for attention instead, which
        // VoiceOver says as well (an announcement from the background it does not).
        center.getNotificationSettings { settings in
            let allowed = settings.authorizationStatus == .authorized
            let denied = settings.authorizationStatus == .denied
            DispatchQueue.main.async {
                self.allowed = allowed
                if denied, !NSApp.isActive { NSApp.requestUserAttention(.informationalRequest) }
            }
        }
        let content = UNMutableNotificationContent()
        content.title = notice.title
        content.body = notice.text
        content.sound = .default
        var info: [String: Any] = [:]
        switch action {
        case .transfer(let id, let path):
            info["transfer"] = String(id)
            if let path = path {
                info["path"] = path
                content.categoryIdentifier = "saved"
            }
        case .offer(let id):
            info["offer"] = String(id)
        default:
            break
        }
        content.userInfo = info
        var identifier = UUID().uuidString
        if case .offer(let id) = action { identifier = offerIdentifier(id) }
        center.add(
            UNNotificationRequest(identifier: identifier, content: content, trigger: nil))
    }

    private func offerIdentifier(_ id: UInt64) -> String {
        "offer-\(id)"
    }

    /// An offer that was answered, or withdrawn, has nothing left to be clicked for.
    func forgetOffer(_ id: UInt64) {
        center?.removeDeliveredNotifications(withIdentifiers: [offerIdentifier(id)])
    }

    // With the Transfers window in front, its rows and VoiceOver say how transfers
    // are doing; otherwise it shows as it would in the background.
    func userNotificationCenter(
        _: UNUserNotificationCenter, willPresent notification: UNNotification,
        withCompletionHandler done: @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        if transfersWindowIsInFront, notification.request.content.userInfo["transfer"] != nil {
            done([])
        } else {
            done([.banner, .list, .sound])
        }
    }

    func userNotificationCenter(
        _: UNUserNotificationCenter, didReceive response: UNNotificationResponse,
        withCompletionHandler done: @escaping () -> Void
    ) {
        let info = response.notification.request.content.userInfo
        let path = info["path"] as? String
        switch response.actionIdentifier {
        case "open":
            if let path = path { open(.open(path)) }
        case "reveal":
            if let path = path { open(.reveal(path)) }
        default:
            if let path = path {
                open(.reveal(path))
            } else if let text = info["transfer"] as? String, let id = UInt64(text) {
                open(.transfer(id, path: nil))
            } else if let text = info["offer"] as? String, let id = UInt64(text) {
                open(.offer(id))
            } else {
                open(.none)
            }
        }
        done()
    }
}
