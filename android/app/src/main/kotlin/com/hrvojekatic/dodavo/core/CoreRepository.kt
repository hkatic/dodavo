package com.hrvojekatic.dodavo.core

import java.util.concurrent.Executors
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.asCoroutineDispatcher
import kotlinx.coroutines.channels.BufferOverflow
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.builtins.serializer
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonPrimitive

/** Where the core is. */
sealed interface CoreState {
    data object Starting : CoreState

    data class Running(val started: Started) : CoreState

    /** Nothing can go on: this says why. */
    data class Failed(val reason: String) : CoreState
}

/** The online status line. */
data class Status(val online: Boolean, val text: String)

/** Something to say aloud, without moving the focus. `serial` tells repeats apart. */
data class Announcement(val text: String, val serial: Long)

/** What the app tells outside its screens (notifications). */
sealed interface Event {
    data class TransferNotice(val id: Long, val notice: Notice, val path: String?) : Event

    data class OfferArrived(val offer: Offer, val notice: Notice) : Event

    data class OfferGone(val id: Long, val notice: Notice) : Event

    /** An offer was declined for its size; [serial] tells one from the next. */
    data class OfferDeclined(val serial: Long, val notice: Notice) : Event

    data class Error(val text: String) : Event

    /** No send reads the document [uri] any more. */
    data class DocumentDone(val uri: String) : Event
}

/** The answer of a call that can fail: `{"error": …}` or `{"problem": …}`. */
@Serializable
private data class Reply(val error: String? = null, val problem: String? = null, val confirm: String? = null)

/** The answer of `block`. */
@Serializable
private data class BlockReply(val error: String? = null, val declined: List<Long> = emptyList())

/** What [CoreRepository.checkAccept] found. */
sealed interface AcceptCheck {
    data object Fine : AcceptCheck

    data class Problem(val text: String) : AcceptCheck

    /** It may not fit: accept anyway? */
    data class Confirm(val text: String) : AcceptCheck
}

/**
 * The single source of truth of the app: what the core says, kept as state,
 * and what the user does, passed on. One thread waits for the core's messages.
 */
class CoreRepository(
    private val core: DodavoCore,
    private val scope: CoroutineScope,
    private val io: CoroutineDispatcher = Dispatchers.IO,
) {
    private val _state = MutableStateFlow<CoreState>(CoreState.Starting)
    val state: StateFlow<CoreState> = _state.asStateFlow()

    private val _status = MutableStateFlow<Status?>(null)
    val status: StateFlow<Status?> = _status.asStateFlow()

    private val _transfers = MutableStateFlow<Map<Long, Row>>(emptyMap())
    /** The transfers by id; newer ones have higher ids. */
    val transfers: StateFlow<Map<Long, Row>> = _transfers.asStateFlow()

    private val _offers = MutableStateFlow<Map<Long, Offer>>(emptyMap())
    val offers: StateFlow<Map<Long, Offer>> = _offers.asStateFlow()

    private val _contacts = MutableStateFlow<List<Contact>>(emptyList())
    val contacts: StateFlow<List<Contact>> = _contacts.asStateFlow()

    private val _blocked = MutableStateFlow<List<Contact>>(emptyList())
    val blocked: StateFlow<List<Contact>> = _blocked.asStateFlow()

    private val _settings = MutableStateFlow<Settings?>(null)
    val settings: StateFlow<Settings?> = _settings.asStateFlow()

    private val _busy = MutableStateFlow(false)
    /** Data moves: the app has to keep running. */
    val busy: StateFlow<Boolean> = _busy.asStateFlow()

    private val _announcement = MutableStateFlow<Announcement?>(null)
    val announcement: StateFlow<Announcement?> = _announcement.asStateFlow()
    private var announcements = 0L

    private var declined = 0L

    private val _line = MutableStateFlow("")
    /** The status line: the latest of the online status and what the core announces. */
    val line: StateFlow<String> = _line.asStateFlow()

    private val _events = MutableSharedFlow<Event>(
        extraBufferCapacity = 64,
        onBufferOverflow = BufferOverflow.DROP_OLDEST,
    )
    /** For notifications; also when no screen is shown. */
    val events: SharedFlow<Event> = _events.asSharedFlow()

    private val listener = Executors.newSingleThreadExecutor { Thread(it, "dodavo-messages") }
        .asCoroutineDispatcher()

    /** Starts the core, and from then on takes its messages. */
    fun start(options: StartOptions) {
        scope.launch(io) {
            val started = runCatching { core.start(options) }
                .getOrElse { Started(fatal = it.message ?: it.toString()) }
            val fatal = started.fatal
            if (fatal != null) {
                _state.value = CoreState.Failed(fatal)
                return@launch
            }
            if (started.alreadyRunning) {
                _state.value = CoreState.Failed("Dodavo runs already.")
                return@launch
            }
            refreshContacts()
            refreshBlocked()
            refreshSettings()
            // Before the screens show the line, so that it is there with
            // them and is no change that TalkBack reads.
            _line.value = started.status
            _state.value = CoreState.Running(started)
            scope.launch(listener) {
                while (isActive) {
                    val message = runCatching { core.next(1_000) }.getOrNull() ?: continue
                    handle(message)
                }
            }
        }
    }

    internal fun handle(message: Message) {
        when (message) {
            is Message.Status -> {
                _status.value = Status(message.online, message.text)
                _line.value = message.text
            }
            is Message.RowChanged -> {
                _transfers.update { it + (message.row.id to message.row) }
                message.announce?.let(::announce)
            }
            is Message.RowGone -> _transfers.update { it - message.id }
            is Message.TransferNotice -> _events.tryEmit(Event.TransferNotice(message.id, message.notice, message.path))
            is Message.OfferArrived -> {
                _offers.update { it + (message.offer.id to message.offer) }
                // Said in the app as well: the offer does not take the focus.
                announce(message.notice.text)
                _events.tryEmit(Event.OfferArrived(message.offer, message.notice))
            }
            is Message.OfferGone -> {
                _offers.update { it - message.id }
                announce(message.notice.text)
                _events.tryEmit(Event.OfferGone(message.id, message.notice))
            }
            is Message.OfferDeclined -> {
                // Said in the app as well: nothing on the screen shows it.
                announce(message.notice.text)
                declined += 1
                _events.tryEmit(Event.OfferDeclined(declined, message.notice))
            }
            is Message.Busy -> _busy.value = message.busy
            is Message.DocumentDone -> _events.tryEmit(Event.DocumentDone(message.uri))
            is Message.Error -> _events.tryEmit(Event.Error(message.text))
            is Message.Fatal -> _state.value = CoreState.Failed(message.text)
        }
    }

    private fun announce(text: String) {
        announcements += 1
        _announcement.value = Announcement(text, announcements)
        _line.value = text
    }

    private suspend fun call(call: Call): String = withContext(io) { core.call(call) }

    private suspend fun reply(call: Call): Reply =
        coreJson.decodeFromString(Reply.serializer(), call(call).takeIf { it != "null" } ?: "{}")

    private suspend fun refreshContacts() {
        _contacts.value = coreJson.decodeFromString(ListSerializer(Contact.serializer()), call(Call.Contacts))
    }

    private suspend fun refreshBlocked() {
        _blocked.value = coreJson.decodeFromString(ListSerializer(Contact.serializer()), call(Call.Blocked))
    }

    private suspend fun refreshSettings() {
        _settings.value = coreJson.decodeFromString(Settings.serializer(), call(Call.GetSettings))
    }

    /** Sends each of [paths] to [peerId] as a transfer of its own; an error text if that cannot be. */
    suspend fun send(peerId: String, paths: List<String>): String? =
        reply(Call.Send(peerId, paths)).error

    /** Sends each of [documents] to [peerId], read where they are; an error text if that cannot be. */
    suspend fun sendDocuments(peerId: String, documents: List<Document>): String? =
        reply(Call.SendDocuments(peerId, documents)).error

    /** The URIs of the documents that sends still read. */
    suspend fun documentsInUse(): List<String> =
        coreJson.decodeFromString(ListSerializer(String.serializer()), call(Call.DocumentsInUse))

    suspend fun checkAccept(id: Long): AcceptCheck {
        val reply = reply(Call.CheckAccept(id))
        return when {
            reply.problem != null -> AcceptCheck.Problem(reply.problem)
            reply.confirm != null -> AcceptCheck.Confirm(reply.confirm)
            else -> AcceptCheck.Fine
        }
    }

    suspend fun space(id: Long): Space? =
        call(Call.CheckSpace(id)).takeIf { it != "null" }?.let { coreJson.decodeFromString(Space.serializer(), it) }

    suspend fun accept(id: Long) {
        _offers.update { it - id }
        call(Call.Accept(id))
    }

    suspend fun decline(id: Long) {
        _offers.update { it - id }
        call(Call.Decline(id))
    }

    suspend fun pause(id: Long) { call(Call.Pause(id)) }

    suspend fun resume(id: Long) { call(Call.Resume(id)) }

    suspend fun cancel(id: Long) { call(Call.Cancel(id)) }

    suspend fun removeRow(id: Long) { call(Call.RemoveRow(id)) }

    suspend fun clearFinished() { call(Call.ClearFinished) }

    /**
     * Why a recipient cannot be added (or edited, from [editing]) like this; null if it can.
     * With [limit], also why that cannot be their limit.
     */
    suspend fun recipientProblem(
        name: String,
        contactId: String,
        editing: String? = null,
        limit: RecipientLimit? = null,
    ): String? = reply(Call.CheckRecipient(name, contactId, editing, limit)).problem

    /** Without [limit], the recipient is under the general one. */
    suspend fun addRecipient(name: String, contactId: String, limit: RecipientLimit? = null): String? =
        reply(Call.AddRecipient(name, contactId, limit)).error.also { refreshContacts(); refreshBlocked() }

    /** Without [limit], the recipient keeps the one they have. */
    suspend fun editRecipient(oldId: String, name: String, contactId: String, limit: RecipientLimit? = null): String? =
        reply(Call.EditRecipient(oldId, name, contactId, limit)).error.also { refreshContacts(); refreshBlocked() }

    suspend fun removeRecipient(contactId: String): String? =
        reply(Call.RemoveRecipient(contactId)).error.also { refreshContacts() }

    /** Blocks a sender; their waiting offers are declined. An error text if the settings could not be saved. */
    suspend fun block(name: String, contactId: String): String? {
        val reply = coreJson.decodeFromString(BlockReply.serializer(), call(Call.Block(name, contactId)))
        _offers.update { offers -> offers - reply.declined.toSet() }
        refreshContacts()
        refreshBlocked()
        return reply.error
    }

    suspend fun unblock(contactId: String): String? =
        reply(Call.Unblock(contactId)).error.also { refreshBlocked() }

    /** Why the settings cannot be like this; null if they can. */
    suspend fun settingsProblem(settings: Settings): String? = reply(Call.SettingsProblem(settings)).problem

    /** The recipients follow: a limit of their own starts from the general one. */
    suspend fun setSettings(settings: Settings): String? =
        reply(Call.SetSettings(settings)).error.also { refreshSettings(); refreshContacts() }

    /** Dodavo's license and those of its components. */
    suspend fun licenses(): Pair<String, String> {
        val reply = coreJson.decodeFromString(JsonObject.serializer(), call(Call.Licenses))
        return reply.getValue("dodavo").jsonPrimitive.content to reply.getValue("components").jsonPrimitive.content
    }

    suspend fun networkChanged() { call(Call.NetworkChanged) }

    private var language: String? = null

    /** The core speaks [code] from now on, if it did not already. */
    suspend fun useLanguage(code: String) {
        val started = state.first { it !is CoreState.Starting } as? CoreState.Running ?: return
        if ((language ?: started.started.language) == code) return
        language = code
        call(Call.SetLanguage(code))
    }

    /** Suspends the transfers; they go on at the next start. */
    suspend fun shutdown() { call(Call.Shutdown) }
}
