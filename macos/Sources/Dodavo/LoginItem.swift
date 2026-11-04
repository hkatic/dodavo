import AppKit
import ServiceManagement

/// Open at Login. Since macOS 13 that is `SMAppService`; before, the shared list
/// of login items that System Preferences shows.
enum LoginItem {
    /// Whether this start came from the login items.
    static var launchedAtLogin: Bool {
        guard let event = NSAppleEventManager.shared().currentAppleEvent else { return false }
        return event.eventID == kAEOpenApplication
            && event.paramDescriptor(forKeyword: keyAEPropData)?.enumCodeValue
                == keyAELaunchedAsLogInItem
    }

    /// What macOS says, where it says it: the user can change this in System
    /// Settings as well. `nil` on older systems, where only the settings know.
    static var isEnabled: Bool? {
        if #available(macOS 13, *) {
            return SMAppService.mainApp.status == .enabled
        }
        return nil
    }

    /// Makes it so; an error text when that fails.
    static func set(_ enabled: Bool) -> String? {
        if #available(macOS 13, *) {
            do {
                if enabled {
                    try SMAppService.mainApp.register()
                } else {
                    try SMAppService.mainApp.unregister()
                }
                return nil
            } catch {
                return error.localizedDescription
            }
        }
        return legacySet(enabled)
    }

    private static func legacySet(_ enabled: Bool) -> String? {
        guard
            let list = LSSharedFileListCreate(
                nil, kLSSharedFileListSessionLoginItems.takeUnretainedValue(), nil)?
                .takeRetainedValue()
        else { return "LSSharedFileListCreate" }
        let bundle = Bundle.main.bundleURL as CFURL
        var seed: UInt32 = 0
        let items =
            LSSharedFileListCopySnapshot(list, &seed)?.takeRetainedValue()
            as? [LSSharedFileListItem] ?? []
        for item in items {
            var error: Unmanaged<CFError>?
            let url = LSSharedFileListItemCopyResolvedURL(item, 0, &error)?.takeRetainedValue()
            error?.release()
            if (url as URL?)?.path == (bundle as URL).path {
                LSSharedFileListItemRemove(list, item)
            }
        }
        if enabled {
            LSSharedFileListInsertItemURL(
                list, kLSSharedFileListItemLast.takeUnretainedValue(), nil, nil, bundle, nil, nil)
        }
        return nil
    }
}
