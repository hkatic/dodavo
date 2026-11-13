import AppKit

/// Plays Dodavo's sounds, which the library asks for as the settings say
/// (`Message.sound`). One sound plays at a time: a new one takes the place of
/// one that still sounds.
enum Sounds {
    private static var playing: NSSound?
    private static var played = Date.distantPast

    /// Plays `sound`. A file that cannot be played gives the built-in sound,
    /// so that the event is not missed.
    static func play(_ sound: SoundInfo) {
        playing?.stop()
        playing = nil
        var next = sound.file.flatMap { NSSound(contentsOfFile: $0, byReference: true) }
        if next?.play() != true {
            next = Core.builtInSound(sound.event).flatMap { NSSound(data: $0) }
            guard next?.play() == true else { return }
        }
        playing = next
        played = Date()
    }

    /// Whether `path` is a sound that can be played here.
    static func canPlay(_ path: String) -> Bool {
        NSSound(contentsOfFile: path, byReference: true) != nil
    }

    /// Whether one of Dodavo's sounds began a moment ago: the notification
    /// that tells of the same event then leaves out the system's own sound.
    static var justPlayed: Bool {
        Date().timeIntervalSince(played) < 0.5
    }
}
