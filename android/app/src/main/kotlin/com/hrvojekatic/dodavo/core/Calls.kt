package com.hrvojekatic.dodavo.core

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/** What the interface asks of the core (`core::Call` in Rust). */
@Serializable
sealed interface Call {
    @Serializable
    @SerialName("send")
    data class Send(@SerialName("peer_id") val peerId: String, val paths: List<String>) : Call

    /** Whether an offer fits into Downloads. */
    @Serializable
    @SerialName("space")
    data class CheckSpace(val id: Long) : Call

    /** What stands in the way of accepting an offer: `{problem}` or `{confirm}`. */
    @Serializable
    @SerialName("check_accept")
    data class CheckAccept(val id: Long) : Call

    @Serializable
    @SerialName("accept")
    data class Accept(val id: Long) : Call

    @Serializable
    @SerialName("decline")
    data class Decline(val id: Long) : Call

    @Serializable
    @SerialName("pause")
    data class Pause(val id: Long) : Call

    @Serializable
    @SerialName("resume")
    data class Resume(val id: Long) : Call

    @Serializable
    @SerialName("cancel")
    data class Cancel(val id: Long) : Call

    @Serializable
    @SerialName("remove_row")
    data class RemoveRow(val id: Long) : Call

    @Serializable
    @SerialName("clear_finished")
    data object ClearFinished : Call

    @Serializable
    @SerialName("cancel_all")
    data object CancelAll : Call

    @Serializable
    @SerialName("contacts")
    data object Contacts : Call

    /** `{problem}` for a recipient that cannot be added (or edited) like this. */
    @Serializable
    @SerialName("check_recipient")
    data class CheckRecipient(
        val name: String,
        @SerialName("contact_id") val contactId: String,
        val editing: String? = null,
    ) : Call

    @Serializable
    @SerialName("add_recipient")
    data class AddRecipient(val name: String, @SerialName("contact_id") val contactId: String) : Call

    @Serializable
    @SerialName("edit_recipient")
    data class EditRecipient(
        @SerialName("old_id") val oldId: String,
        val name: String,
        @SerialName("contact_id") val contactId: String,
    ) : Call

    @Serializable
    @SerialName("remove_recipient")
    data class RemoveRecipient(@SerialName("contact_id") val contactId: String) : Call

    /** `{error, declined}`: the offers of theirs that it declined. */
    @Serializable
    @SerialName("block")
    data class Block(val name: String, @SerialName("contact_id") val contactId: String) : Call

    @Serializable
    @SerialName("unblock")
    data class Unblock(@SerialName("contact_id") val contactId: String) : Call

    @Serializable
    @SerialName("blocked")
    data object Blocked : Call

    @Serializable
    @SerialName("settings")
    data object GetSettings : Call

    @Serializable
    @SerialName("set_settings")
    data class SetSettings(val settings: Settings) : Call

    @Serializable
    @SerialName("settings_problem")
    data class SettingsProblem(val settings: Settings) : Call

    @Serializable
    @SerialName("licenses")
    data object Licenses : Call

    /** Suspends the transfers and closes the network; blocks up to 3 s. */
    @Serializable
    @SerialName("shutdown")
    data object Shutdown : Call

    @Serializable
    @SerialName("network_changed")
    data object NetworkChanged : Call

    @Serializable
    @SerialName("set_language")
    data class SetLanguage(val code: String) : Call
}
