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
)

/** A recipient, or a blocked sender. */
@Serializable
data class Contact(val name: String, val id: String)

/** What is kept of a received folder. */
@Serializable
enum class ReceivedFolder {
    @SerialName("extract") Extract,
    @SerialName("keep_zip") KeepZip,
    @SerialName("both") Both,
}

/** The settings as the Settings screen shows them. */
@Serializable
data class Settings(
    @SerialName("my_name") val myName: String,
    @SerialName("auto_accept") val autoAccept: Boolean,
    @SerialName("received_folder") val receivedFolder: ReceivedFolder,
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

    /** Whether data moves, so that the app keeps running. */
    @Serializable
    @SerialName("busy")
    data class Busy(val busy: Boolean) : Message

    @Serializable
    @SerialName("error")
    data class Error(val text: String) : Message

    /** Nothing can go on: this says why. */
    @Serializable
    @SerialName("fatal")
    data class Fatal(val text: String) : Message
}
