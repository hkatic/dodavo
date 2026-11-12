package com.hrvojekatic.dodavo.ui.recipients

import android.net.Uri
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.hrvojekatic.dodavo.core.Contact
import com.hrvojekatic.dodavo.core.CoreRepository
import com.hrvojekatic.dodavo.core.CoreState
import com.hrvojekatic.dodavo.core.Sending
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch

/** A recipient, with what their transfer is doing. */
data class RecipientItem(val contact: Contact, val transfer: String?)

data class RecipientsUiState(
    val state: CoreState = CoreState.Starting,
    val line: String = "",
    val myId: String = "",
    val recipients: List<RecipientItem> = emptyList(),
    /** Something went wrong, to say until the user closes it. */
    val message: String? = null,
)

class RecipientsViewModel(
    private val repository: CoreRepository,
    private val sending: Sending,
) : ViewModel() {
    private val message = MutableStateFlow<String?>(null)

    val uiState: StateFlow<RecipientsUiState> = combine(
        repository.state,
        repository.line,
        repository.contacts,
        repository.transfers,
        message,
    ) { state, line, contacts, transfers, message ->
        val running = transfers.values.filter { it.active }
        RecipientsUiState(
            state = state,
            line = line,
            myId = (state as? CoreState.Running)?.started?.myId.orEmpty(),
            recipients = contacts.map { contact ->
                RecipientItem(
                    contact,
                    running.lastOrNull { it.peerId.equals(contact.id, ignoreCase = true) }?.state,
                )
            },
            message = message,
        )
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), RecipientsUiState())

    /** Sends each of the files to each of [contacts], every one as a transfer of its own. */
    fun sendFiles(contacts: List<Contact>, uris: List<Uri>) {
        if (uris.isEmpty() || contacts.isEmpty()) return
        viewModelScope.launch {
            message.value = runCatching { sending.sendFiles(contacts.map { it.id }, uris) }
                .getOrElse { it.message }
        }
    }

    fun sendFolder(contacts: List<Contact>, tree: Uri) {
        if (contacts.isEmpty()) return
        viewModelScope.launch {
            message.value = runCatching { sending.sendFolder(contacts.map { it.id }, tree) }
                .getOrElse { it.message }
        }
    }

    fun remove(contact: Contact) {
        viewModelScope.launch { message.value = repository.removeRecipient(contact.id) }
    }

    fun block(contact: Contact) {
        viewModelScope.launch { message.value = repository.block(contact.name, contact.id) }
    }

    fun messageShown() {
        message.value = null
    }
}
