import AppKit

// Keeps the delegate for the life of the program: NSApplication holds it weakly.
let delegate = AppDelegate()
let application = NSApplication.shared
application.delegate = delegate
application.setActivationPolicy(.regular)
application.run()
