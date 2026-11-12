package com.hrvojekatic.dodavo.ui.recipients

import android.net.Uri
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.hrvojekatic.dodavo.R
import com.hrvojekatic.dodavo.core.Contact
import com.hrvojekatic.dodavo.ui.Navigator
import com.hrvojekatic.dodavo.ui.TransfersKey
import com.hrvojekatic.dodavo.ui.common.MessageDialog
import com.hrvojekatic.dodavo.ui.common.StatusLine
import com.hrvojekatic.dodavo.ui.common.container
import com.hrvojekatic.dodavo.ui.common.shortId
import kotlinx.coroutines.launch

/**
 * Items shared from another app: the user chooses who gets them. A tap on a
 * recipient sends to that one; Select in the top bar, or a long press, lets
 * several be chosen, and Send then sends to all of them. The items are copied
 * at once, while this screen still may read them.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SendToScreen(uris: List<String>, navigator: Navigator) {
    val container = LocalContext.current.container
    val contacts by container.repository.contacts.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()
    var busy by remember { mutableStateOf(false) }
    var message by remember { mutableStateOf<String?>(null) }
    var selecting by rememberSaveable { mutableStateOf(false) }
    // The Contact IDs of the selected recipients.
    var selected by rememberSaveable { mutableStateOf(listOf<String>()) }
    // In the order of the list, and without those who are no longer in it.
    val chosen = contacts.filter { it.id in selected }
    fun leaveSelection() {
        selecting = false
        selected = emptyList()
    }
    fun toggle(contact: Contact) {
        val ids = chosen.map { it.id }
        selected = if (contact.id in ids) ids - contact.id else ids + contact.id
        selecting = true
    }
    fun send(to: List<Contact>) {
        busy = true
        scope.launch {
            val error = runCatching {
                container.sending.sendFiles(to.map { it.id }, uris.map(Uri::parse), shared = true)
            }.getOrElse { it.message }
            busy = false
            if (error != null) message = error else navigator.top(TransfersKey)
        }
    }
    BackHandler(enabled = selecting) { leaveSelection() }

    Scaffold(
        topBar = {
            if (selecting) {
                SelectionTopBar(count = chosen.size, onClose = ::leaveSelection) {
                    IconButton(onClick = { send(chosen) }, enabled = chosen.isNotEmpty() && !busy) {
                        Icon(
                            Icons.AutoMirrored.Filled.Send,
                            contentDescription = stringResource(R.string.action_send),
                        )
                    }
                }
            } else TopAppBar(
                navigationIcon = {
                    IconButton(onClick = navigator::back) {
                        Icon(Icons.Filled.Close, contentDescription = stringResource(R.string.close))
                    }
                },
                title = { Text(stringResource(R.string.action_send_files)) },
                actions = {
                    if (contacts.isNotEmpty()) {
                        TextButton(onClick = { selecting = true }) {
                            Text(stringResource(R.string.action_select))
                        }
                    }
                },
            )
        },
    ) { padding ->
        Column(Modifier.fillMaxSize().padding(padding)) {
            if (contacts.isEmpty()) {
                StatusLine(stringResource(R.string.recipients_empty))
            }
            LazyColumn {
                items(contacts, key = { it.id }) { contact ->
                    RecipientListItem(
                        name = contact.name,
                        supporting = shortId(contact.id),
                        selecting = selecting,
                        selected = contact.id in selected,
                        onClick = { send(listOf(contact)) },
                        onToggle = { toggle(contact) },
                        enabled = !busy,
                    )
                    HorizontalDivider()
                }
            }
        }
    }
    message?.let { MessageDialog(it, onDismiss = { message = null }) }
}
