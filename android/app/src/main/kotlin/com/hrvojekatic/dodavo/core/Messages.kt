package com.hrvojekatic.dodavo.core

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/** What the core says when it has started (`core::Started` in Rust). */
@Serializable
data class Started(
    /** Nothing can go on: this says why. */
    val fatal: String? = null,
    @SerialName("already_running") val alreadyRunning: Boolean = false,
    /** The settings could not be read: this says so. */
    val warning: String? = null,
    @SerialName("my_id") val myId: String = "",
    /** What the status line says until the network says more. */
    val status: String = "",
    val profile: String? = null,
    val language: String = "en",
    val version: String = "",
    val author: String = "",
    /** The years of the copyright notice. */
    val copyright: String = "",
    val website: String = "",
    /** Each language by its code, with its own name for it. */
    val languages: List<List<String>> = emptyList(),
)

/** What the user is told about a transfer. */
@Serializable
data class Notice(val title: String, val text: String)

/** A transfer as the interface shows it (`view::Row`). */
@Serializable
data class Row(
    val id: Long,
    @SerialName("peer_id") val peerId: String,
    val peer: String,
    val sending: Boolean,
    val item: String,
    val active: Boolean,
    val succeeded: Boolean,
    val status: String,
    val state: String,
    /** How much is done, 0 to 1; null while there is nothing to count. */
    val fraction: Double? = null,
    val amount: String,
    val speed: String,
    val remaining: String,
    val detail: String,
    @SerialName("can_pause") val canPause: Boolean,
    val paused: Boolean,
    @SerialName("can_cancel") val canCancel: Boolean,
    /** Where a received item was saved. */
    val path: String? = null,
    @SerialName("moving_data") val movingData: Boolean,
)

/** Whether an offer fits where it would go. */
@Serializable
data class Space(val text: String, val fits: Boolean, val enough: Boolean? = null)

/** Someone wants to send something. */
@Serializable
data class Offer(
    val id: Long,
    @SerialName("from_id") val fromId: String,
    @SerialName("from_name") val fromName: String,
    val known: Boolean,
    val headline: String,
    val description: String,
    @SerialName("unknown_warning") val unknownWarning: String? = null,
    @SerialName("add_label") val addLabel: String? = null,
    val dir: String,
    val space: Space,
    /** Says that the item is over the sender's limit, when it is. */
    @SerialName("over_limit") val overLimit: String? = null,
)

/**
 * A document that the user chose, as the core reads it (`source::Document`):
 * a file, or a folder with what it holds ([items]), each folder before what
 * is in it. Times in milliseconds since 1970.
 */
@Serializable
data class Document(
    val uri: String,
    val name: String,
    val modified: Long? = null,
    val items: List<DocumentItem>? = null,
)

/** A file or folder in a [Document] that is a folder: its names from the folder down. */
@Serializable
data class DocumentItem(
    val path: List<String>,
    val folder: Boolean = false,
    val size: Long = 0,
    val modified: Long? = null,
    val uri: String = "",
)

/**
 * How large an item a recipient may send: as for everyone ([kind] "general"),
 * any size ("none") or a limit of their own ("own"), whose size is [number]
 * of [unit] ("mb" or "gb"). For the other kinds, that is the size to offer
 * when the user chooses a limit of their own.
 */
@Serializable
data class RecipientLimit(val kind: String, val number: String, val unit: String) {
    companion object {
        const val GENERAL = "general"
        const val NONE = "none"
        const val OWN = "own"
    }
}

/** A recipient, or a blocked sender (who has no [limit]). */
@Serializable
data class Contact(val name: String, val id: String, val limit: RecipientLimit? = null)

/** What is kept of a received folder. */
@Serializable
enum class ReceivedFolder {
    @SerialName("extract") Extract,
    @SerialName("keep_zip") KeepZip,
    @SerialName("both") Both,
}

/** What happens to an offered item that is over its sender's limit. */
@Serializable
enum class OverLimit {
    @SerialName("ask") Ask,
    @SerialName("decline") Decline,
    @SerialName("decline_and_tell") DeclineAndTell,
}

/**
 * The settings as the Settings screen shows them. The general limit on the
 * size of an item is [limitNumber] of [limitUnit] ("mb" or "gb"), as typed,
 * and holds while [limitSize] is on. None of them has a default: the core
 * needs every one of them back.
 */
@Serializable
data class Settings(
    @SerialName("my_name") val myName: String,
    @SerialName("auto_accept") val autoAccept: Boolean,
    @SerialName("received_folder") val receivedFolder: ReceivedFolder,
    @SerialName("limit_size") val limitSize: Boolean,
    @SerialName("limit_number") val limitNumber: String,
    @SerialName("limit_unit") val limitUnit: String,
    @SerialName("over_limit") val overLimit: OverLimit,
)

/** What the core says, as it happens (`core::Out` in Rust). */
@Serializable
sealed interface Message {
    @Serializable
    @SerialName("status")
    data class Status(val online: Boolean, val text: String) : Message

    /** A transfer's new state, and a change to say aloud. */
    @Serializable
    @SerialName("row")
    data class RowChanged(val row: Row, val announce: String? = null) : Message

    @Serializable
    @SerialName("row_gone")
    data class RowGone(val id: Long) : Message

    @Serializable
    @SerialName("notice")
    data class TransferNotice(val id: Long, val notice: Notice, val path: String? = null) : Message

    @Serializable
    @SerialName("offer")
    data class OfferArrived(val offer: Offer, val notice: Notice) : Message

    /** The sender gave up while the user was still deciding. */
    @Serializable
    @SerialName("offer_gone")
    data class OfferGone(val id: Long, val notice: Notice) : Message

    /** An offer was declined for its size, and the user wants to be told. */
    @Serializable
    @SerialName("declined")
    data class OfferDeclined(val notice: Notice) : Message

    /** Whether data moves, so that the app keeps running. */
    @Serializable
    @SerialName("busy")
    data class Busy(val busy: Boolean) : Message

    /** No send reads the document [uri] any more. */
    @Serializable
    @SerialName("document_done")
    data class DocumentDone(val uri: String) : Message

    @Serializable
    @SerialName("error")
    data class Error(val text: String) : Message

    /** Nothing can go on: this says why. */
    @Serializable
    @SerialName("fatal")
    data class Fatal(val text: String) : Message
}
