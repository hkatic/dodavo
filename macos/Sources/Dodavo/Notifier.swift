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

    init(open: @escaping (Action) -> Void) {
        self.open = open
        super.init()
        center?.delegate = self
        let openItem = UNNotificationAction(identifier: "open", title: L[.open], options: .foreground)
        let reveal = UNNotificationAction(
            identifier: "reveal", title: L[.show_in_finder], options: .foreground)
        center?.setNotificationCategories([
            UNNotificationCategory(
                identifier: "saved", actions: [openItem, reveal], intentIdentifiers: [],
                options: [])
        ])
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
            guard settings.authorizationStatus == .denied else { return }
            DispatchQueue.main.async {
                if !NSApp.isActive { NSApp.requestUserAttention(.informationalRequest) }
            }
        }
        let content = UNMutableNotificationContent()
        content.title = notice.title
        content.body = notice.text
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
        center.add(
            UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil))
    }

    // While Dodavo is in front with its window open, the window and VoiceOver
    // say it; otherwise it shows as it would in the background.
    func userNotificationCenter(
        _: UNUserNotificationCenter, willPresent notification: UNNotification,
        withCompletionHandler done: @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        if NSApp.isActive, NSApp.keyWindow?.identifier?.rawValue == "transfers" {
            let content = notification.request.content
            announce("\(content.title): \(content.body)")
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
