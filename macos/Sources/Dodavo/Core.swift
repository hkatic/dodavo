import CDodavo
import Foundation

/// What Rust says when the program starts.
struct Started: Decodable {
    let fatal: String?
    let alreadyRunning: Bool
    let warning: String?
    let myId: String
    let profile: String?
    let language: String
    let version: String
    let copyright: String
    let website: String
    let donateUrl: String
    let startHidden: Bool
    let showInMenuBar: Bool
    let openAtLogin: Bool
    /// The longest chat message that can be written, in characters.
    let chatMaxChars: Int
    let languages: [[String]]
    /// Where Dodavo's log is; empty where there is none.
    let logPath: String
}

/// How large an item a recipient may send: as for everyone ("general"), any
/// size ("none") or a limit of their own ("own"), whose size is `number` of
/// `unit` ("mb" or "gb"). For the other kinds, that is the size to offer when
/// the user chooses a limit of their own.
struct RecipientLimit: Decodable, Equatable {
    let kind: String
    let number: String
    let unit: String
}

struct Contact: Decodable, Equatable {
    let name: String
    let id: String
    /// A blocked sender has none.
    let limit: RecipientLimit?
}

struct Row: Decodable, Equatable {
    let id: UInt64
    let peerId: String
    let peer: String
    let sending: Bool
    let item: String
    let active: Bool
    let succeeded: Bool
    let status: String
    let state: String
    let fraction: Double?
    let amount: String
    let speed: String
    let remaining: String
    let detail: String
    let canPause: Bool
    let paused: Bool
    let canCancel: Bool
    let path: String?
    let movingData: Bool
}

struct Notice: Decodable {
    let title: String
    let text: String

    init(title: String, text: String) {
        self.title = title
        self.text = text
    }
}

struct SpaceInfo: Decodable {
    let text: String
    let fits: Bool
    let enough: Bool?
}

struct OfferInfo: Decodable {
    let id: UInt64
    let fromId: String
    let fromName: String
    let known: Bool
    let headline: String
    let description: String
    let unknownWarning: String?
    let addLabel: String?
    let dir: String
    let space: SpaceInfo
    /// Says that the item is over the sender's limit, when it is.
    let overLimit: String?
}

/// What the Incoming Transfer window says of the offers of one sender that
/// wait for one answer.
struct OffersInfo: Decodable {
    let headline: String
    /// The item, or what several come to.
    let description: String
    /// Each item, when there are several.
    let items: [String]
    /// Says that something here is over the sender's limit, when it is.
    let overLimit: String?
    let space: SpaceInfo
}

/// A message of a chat. The window writes its time as this Mac does.
struct ChatEntry: Decodable, Equatable {
    let time: UInt64
    let mine: Bool
    /// A message of the user's that could not be sent.
    var failed: Bool
    let text: String
}

/// A chat as its window shows it.
struct ChatInfo: Decodable {
    /// The other person's Contact ID, as the chat is known by.
    let peer: String
    let name: String
    /// Whether the other person is a recipient.
    let known: Bool
    let title: String
    /// For a person who is not a recipient: who it is.
    let warning: String?
    let entries: [ChatEntry]
    /// Which change of the chats these messages are after. What the library
    /// says of a change carries its number, and a window leaves out what it
    /// has already: one that opens for a message has that message.
    let seq: UInt64
    /// The longest message that can be written, in characters.
    let maxChars: Int
}

/// A sound to play: the built-in one of `event`, or the user's `file` in its
/// place.
struct SoundInfo: Decodable {
    let event: String
    let file: String?
}

/// An event that has a sound, as the settings list it: `kind` is "default"
/// (the built-in sound), "off" or "file", and `shown` says it in words.
struct SoundSetting: Decodable {
    let event: String
    let name: String
    let kind: String
    let file: String?
    let shown: String
}

struct SoundSettings: Decodable {
    let on: Bool
    let events: [SoundSetting]
}

struct Settings: Codable {
    var myName: String
    var openAtLogin: Bool
    var hideAtLogin: Bool
    var showInMenuBar: Bool
    var checkUpdates: Bool
    var language: String?
    var alwaysSave: Bool
    var alwaysSaveDir: String?
    var autoAccept: Bool
    var receivedFolder: String
    /// The folder that the user chose for temporary files, or none for the
    /// system's place.
    var tempDir: String?
    /// Whether to tell of a temporary archive that its volume cannot hold.
    var warnFatTemp: Bool
    /// The general limit on the size of an item: whether there is one, its
    /// size as typed, in `limitUnit` ("mb" or "gb"), and what happens to a
    /// larger item ("ask", "decline" or "decline_and_tell").
    var limitSize: Bool
    var limitNumber: String
    var limitUnit: String
    var overLimit: String
    /// Who may send chat messages: "anyone" (who is not blocked) or "recipients".
    var chatFrom: String
    /// Tell the other person of a chat when the user is writing a message.
    var sendTyping: Bool
    /// Say aloud, and with a sound, that the other person is writing one.
    var announceTyping: Bool
}

/// A message from Rust. An unknown one is decoded as nothing at all.
enum Message: Decodable {
    case status(online: Bool, text: String)
    case row(Row, announce: String?)
    case rowGone(id: UInt64)
    case notice(id: UInt64, notice: Notice, path: String?)
    case offer(OfferInfo, notice: Notice)
    case offerGone(id: UInt64, notice: Notice)
    /// An offer was declined for its size, and the user wants to be told.
    case declined(Notice)
    case busy(Bool)
    case error(String)
    case fatal(String)
    case updateAvailable(headline: String, notes: String, manual: Bool)
    case updateAnswer(String)
    case updateProgress(String, fraction: Double?)
    case updateReady(String)
    case updateFailed(String)
    /// A chat message arrived, and is in the chat with `peer`. `said` is the
    /// message for the status line and for VoiceOver, `notice` what tells of
    /// it while the chat is not in front, and `gone` how many of the oldest
    /// messages left the chat to make room.
    case chatMessage(
        peer: String, name: String, seq: UInt64, entry: ChatEntry, gone: Int, said: String,
        problem: String?, notice: Notice)
    /// A message of the user's is in the chat with `peer`, on its way.
    case chatAdded(peer: String, seq: UInt64, entry: ChatEntry, gone: Int, problem: String?)
    /// A message of the user's was not delivered: the one at `index` of the
    /// chat with `peer`, where its window still has it.
    case chatFailed(
        peer: String, name: String, seq: UInt64, index: Int?, text: String, said: String,
        notice: Notice)
    /// The other person of the chat with `peer` is typing (`text` says so),
    /// or has stopped.
    case chatTyping(peer: String, text: String?, announce: Bool, sound: SoundInfo?)
    case sound(SoundInfo)

    private enum Keys: String, CodingKey {
        case type, online, text, row, announce, id, notice, path, offer, busy
        case headline, notes, manual, fraction
        case peer, name, seq, entry, gone, said, problem, index, sound
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        switch try c.decode(String.self, forKey: .type) {
        case "status":
            self = .status(
                online: try c.decode(Bool.self, forKey: .online),
                text: try c.decode(String.self, forKey: .text))
        case "row":
            self = .row(
                try c.decode(Row.self, forKey: .row),
                announce: try c.decodeIfPresent(String.self, forKey: .announce))
        case "row_gone":
            self = .rowGone(id: try c.decode(UInt64.self, forKey: .id))
        case "notice":
            self = .notice(
                id: try c.decode(UInt64.self, forKey: .id),
                notice: try c.decode(Notice.self, forKey: .notice),
                path: try c.decodeIfPresent(String.self, forKey: .path))
        case "offer":
            self = .offer(
                try c.decode(OfferInfo.self, forKey: .offer),
                notice: try c.decode(Notice.self, forKey: .notice))
        case "offer_gone":
            self = .offerGone(
                id: try c.decode(UInt64.self, forKey: .id),
                notice: try c.decode(Notice.self, forKey: .notice))
        case "declined":
            self = .declined(try c.decode(Notice.self, forKey: .notice))
        case "busy":
            self = .busy(try c.decode(Bool.self, forKey: .busy))
        case "error":
            self = .error(try c.decode(String.self, forKey: .text))
        case "fatal":
            self = .fatal(try c.decode(String.self, forKey: .text))
        case "update_available":
            self = .updateAvailable(
                headline: try c.decode(String.self, forKey: .headline),
                notes: try c.decode(String.self, forKey: .notes),
                manual: try c.decode(Bool.self, forKey: .manual))
        case "update_answer":
            self = .updateAnswer(try c.decode(String.self, forKey: .text))
        case "update_progress":
            self = .updateProgress(
                try c.decode(String.self, forKey: .text),
                fraction: try c.decodeIfPresent(Double.self, forKey: .fraction))
        case "update_ready":
            self = .updateReady(try c.decode(String.self, forKey: .path))
        case "update_failed":
            self = .updateFailed(try c.decode(String.self, forKey: .text))
        case "chat_message":
            self = .chatMessage(
                peer: try c.decode(String.self, forKey: .peer),
                name: try c.decode(String.self, forKey: .name),
                seq: try c.decode(UInt64.self, forKey: .seq),
                entry: try c.decode(ChatEntry.self, forKey: .entry),
                gone: try c.decode(Int.self, forKey: .gone),
                said: try c.decode(String.self, forKey: .said),
                problem: try c.decodeIfPresent(String.self, forKey: .problem),
                notice: try c.decode(Notice.self, forKey: .notice))
        case "chat_added":
            self = .chatAdded(
                peer: try c.decode(String.self, forKey: .peer),
                seq: try c.decode(UInt64.self, forKey: .seq),
                entry: try c.decode(ChatEntry.self, forKey: .entry),
                gone: try c.decode(Int.self, forKey: .gone),
                problem: try c.decodeIfPresent(String.self, forKey: .problem))
        case "chat_failed":
            self = .chatFailed(
                peer: try c.decode(String.self, forKey: .peer),
                name: try c.decode(String.self, forKey: .name),
                seq: try c.decode(UInt64.self, forKey: .seq),
                index: try c.decodeIfPresent(Int.self, forKey: .index),
                text: try c.decode(String.self, forKey: .text),
                said: try c.decode(String.self, forKey: .said),
                notice: try c.decode(Notice.self, forKey: .notice))
        case "chat_typing":
            self = .chatTyping(
                peer: try c.decode(String.self, forKey: .peer),
                text: try c.decodeIfPresent(String.self, forKey: .text),
                announce: try c.decode(Bool.self, forKey: .announce),
                sound: try c.decodeIfPresent(SoundInfo.self, forKey: .sound))
        case "sound":
            self = .sound(try SoundInfo(from: decoder))
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .type, in: c, debugDescription: "an unknown message")
        }
    }
}

/// The Rust library, behind its C functions.
enum Core {
    private static let decoder: JSONDecoder = {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return decoder
    }()

    private static func take(_ text: UnsafeMutablePointer<CChar>?) -> Data? {
        guard let text = text else { return nil }
        defer { dodavo_free(text) }
        return String(cString: text).data(using: .utf8)
    }

    /// Starts the program; `wake` is called, from any thread, when `poll` has
    /// something.
    static func start(
        profile: String?,
        hidden: Bool,
        wake: @escaping @convention(c) (UnsafeMutableRawPointer?) -> Void
    ) -> Started? {
        let options: [String: Any?] = ["profile": profile, "hidden": hidden]
        let json = try? JSONSerialization.data(
            withJSONObject: options.compactMapValues { $0 })
        let text = json.flatMap { String(data: $0, encoding: .utf8) } ?? "{}"
        let reply = take(dodavo_start(text, wake, nil))
        return reply.flatMap { try? decoder.decode(Started.self, from: $0) }
    }

    static func poll() -> Message? {
        guard let data = take(dodavo_poll()) else { return nil }
        return try? decoder.decode(Message.self, from: data)
    }

    /// Whether anything waits, also if what comes next cannot be read.
    static func drain(_ handle: (Message) -> Void) {
        while let data = take(dodavo_poll()) {
            if let message = try? decoder.decode(Message.self, from: data) {
                handle(message)
            }
        }
    }

    private static func raw(_ call: String, _ arguments: [String: Any]) -> Data? {
        var object = arguments
        object["call"] = call
        guard let json = try? JSONSerialization.data(withJSONObject: object),
            let text = String(data: json, encoding: .utf8)
        else { return nil }
        return take(dodavo_call(text))
    }

    /// Does `call`, and gives its answer.
    static func call<T: Decodable>(
        _ call: String, _ arguments: [String: Any] = [:], as type: T.Type = T.self
    ) -> T? {
        raw(call, arguments).flatMap { try? decoder.decode(T.self, from: $0) }
    }

    /// Does `call`, whatever the answer.
    static func perform(_ call: String, _ arguments: [String: Any] = [:]) {
        _ = raw(call, arguments)
    }

    /// A line for Dodavo's log: a text of this app's own, never a name, a
    /// path or what the user wrote (an error by its domain and code only).
    /// `level` is "error", "warn", "info" or "debug".
    static func log(_ level: String, _ message: String) {
        perform("log", ["level": level, "message": message])
    }

    /// `{ "error": … }`, or none.
    static func errorOf(_ call: String, _ arguments: [String: Any]) -> String? {
        struct Reply: Decodable { let error: String? }
        let reply: Reply? = Core.call(call, arguments)
        return reply?.error
    }

    /// `{ "problem": … }`, or none.
    static func problemOf(_ call: String, _ arguments: [String: Any]) -> String? {
        struct Reply: Decodable { let problem: String? }
        let reply: Reply? = Core.call(call, arguments)
        return reply?.problem
    }

    /// The texts of the user interface, by key as they are in Rust.
    static func texts() -> [String: String] {
        guard let data = raw("texts", [:]) else { return [:] }
        return (try? JSONDecoder().decode([String: String].self, from: data)) ?? [:]
    }

    static func settings() -> Settings? {
        call("settings")
    }

    static func settingsJSON(_ settings: Settings) -> [String: Any] {
        let encoder = JSONEncoder()
        encoder.keyEncodingStrategy = .convertToSnakeCase
        guard let data = try? encoder.encode(settings),
            let object = try? JSONSerialization.jsonObject(with: data)
        else { return [:] }
        return ["settings": object]
    }

    static var contacts: [Contact] {
        call("contacts") ?? []
    }
    /// Blocks a sender. The offers of theirs that waited for an answer have been
    /// declined, and their windows are to close.
    static func block(name: String, id: String) -> (error: String?, declined: [UInt64]) {
        struct Reply: Decodable {
            let error: String?
            let declined: [UInt64]
        }
        let reply: Reply? = call("block", ["name": name, "contact_id": id])
        return (reply?.error, reply?.declined ?? [])
    }

    /// Gives this copy a new Contact ID. Every transfer was cancelled for it and
    /// every waiting offer declined, whose windows are to close. `myId` is the
    /// Contact ID that holds now: the old one when there is an error.
    static func changeContactID() -> (error: String?, myId: String?, declined: [UInt64]) {
        struct Reply: Decodable {
            let error: String?
            let myId: String
            let declined: [UInt64]
        }
        let reply: Reply? = call("change_contact_id")
        return (reply?.error, reply?.myId, reply?.declined ?? [])
    }

    /// The blocked senders, in the order of their names.
    static func blocked() -> [Contact] {
        call("blocked") ?? []
    }

    /// The built-in sound of `event`: a WAV file that stays in memory.
    static func builtInSound(_ event: String) -> Data? {
        var length = 0
        guard let bytes = dodavo_sound(event, &length), length > 0 else { return nil }
        return Data(
            bytesNoCopy: UnsafeMutableRawPointer(mutating: bytes), count: length,
            deallocator: .none)
    }
}

/// The texts of the user interface.
enum L {
    private static var table: [String: String] = [:]

    static func load() {
        table = Core.texts()
    }

    static subscript(_ key: K) -> String {
        table[key.rawValue] ?? key.rawValue
    }

    /// Fills the `{name}` placeholders of `text`, each once: what is filled
    /// in is not looked at again, so a name or a message that has braces of
    /// its own stays as it was written.
    private static func fill(_ text: String, _ values: [String: String]) -> String {
        var out = ""
        var rest = Substring(text)
        while let open = rest.firstIndex(of: "{") {
            out += rest[..<open]
            let after = rest[rest.index(after: open)...]
            if let close = after.firstIndex(of: "}"), let value = values[String(after[..<close])] {
                out += value
                rest = after[after.index(after: close)...]
            } else {
                out += "{"
                rest = after
            }
        }
        return out + rest
    }

    /// The text with its `{name}` placeholders filled in.
    static func f(_ key: K, _ values: [String: String]) -> String {
        fill(L[key], values)
    }

    /// The text for the number `n`.
    static func plural(_ key: PluralK, _ n: Int, _ values: [String: String] = [:]) -> String {
        fill(Core.call("plural", ["key": key.rawValue, "n": n], as: String.self) ?? "", values)
    }

    /// `Settings…` on the macOS that calls it that, otherwise `Preferences…`.
    static var settingsMenuItem: String {
        if #available(macOS 13, *) { return L[.menu_settings] }
        return L[.menu_preferences]
    }

    /// The name of Dodavo's settings as its menu says it, for "{settings} > Blocked".
    static var settingsName: String {
        settingsMenuItem.trimmingCharacters(in: CharacterSet(charactersIn: "…").union(.whitespaces))
    }
}
