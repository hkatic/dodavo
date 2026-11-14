package com.hrvojekatic.dodavo.ui.recipients

import android.content.ClipData
import android.content.Intent
import android.os.Build
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.ClipEntry
import androidx.compose.ui.platform.LocalClipboard
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.hrvojekatic.dodavo.R
import com.hrvojekatic.dodavo.core.Contact
import com.hrvojekatic.dodavo.core.CoreState
import com.hrvojekatic.dodavo.ui.AboutKey
import com.hrvojekatic.dodavo.ui.HelpKey
import com.hrvojekatic.dodavo.ui.Navigator
import com.hrvojekatic.dodavo.ui.OfferBanner
import com.hrvojekatic.dodavo.ui.RecipientEditorKey
import com.hrvojekatic.dodavo.ui.SettingsKey
import com.hrvojekatic.dodavo.ui.common.ConfirmDialog
import com.hrvojekatic.dodavo.ui.common.MessageDialog
import com.hrvojekatic.dodavo.ui.common.StatusLine
import com.hrvojekatic.dodavo.ui.common.appViewModel
import com.hrvojekatic.dodavo.ui.common.launchOrTell
import com.hrvojekatic.dodavo.ui.common.shortId
import com.hrvojekatic.dodavo.ui.common.startOrTell
import com.hrvojekatic.dodavo.ui.common.tell
import com.hrvojekatic.dodavo.ui.waitingOffers
import kotlinx.coroutines.launch
import androidx.compose.runtime.rememberCoroutineScope

/** What waits for the user's word about a recipient. */
private sealed interface Pending {
    data class Remove(val contact: Contact) : Pending
    data class Block(val contact: Contact) : Pending
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun RecipientsScreen(navigator: Navigator) {
    val viewModel = appViewModel { RecipientsViewModel(it.repository, it.sending) }
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    val offers = waitingOffers()
    val context = LocalContext.current
    val clipboard = LocalClipboard.current
    val scope = rememberCoroutineScope()
    // Who the picker's result goes to, by Contact ID: kept while Android makes the
    // screen again (a rotation, or the app's process ended while the picker was open).
    var targets by rememberSaveable { mutableStateOf(listOf<String>()) }
    var pending by remember { mutableStateOf<Pending?>(null) }
    var menuOpen by rememberSaveable { mutableStateOf(false) }
    var sendOpen by rememberSaveable { mutableStateOf(false) }
    // The Contact IDs of the selected recipients; the selection is on while there are any.
    var selected by rememberSaveable { mutableStateOf(listOf<String>()) }
    // In the order of the list, and without those who are no longer in it.
    val chosen = state.recipients.map { it.contact }.filter { it.id in selected }
    val selecting = chosen.isNotEmpty()
    fun toggle(contact: Contact) {
        val ids = chosen.map { it.id }
        selected = if (contact.id in ids) ids - contact.id else ids + contact.id
    }
    BackHandler(enabled = selecting) { selected = emptyList() }

    val pickFiles = rememberLauncherForActivityResult(ActivityResultContracts.OpenMultipleDocuments()) { uris ->
        // The selection has done its work once something goes out; a picker
        // that was closed leaves it as it was.
        if (uris.isNotEmpty()) {
            selected = emptyList()
            viewModel.sendFiles(targets, uris)
        }
    }
    val pickFolder = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocumentTree()) { tree ->
        if (tree != null) {
            selected = emptyList()
            viewModel.sendFolder(targets, tree)
        }
    }
    // Some devices have no picker (a maker's build without Files, Android TV).
    fun sendFiles(to: List<Contact>) {
        targets = to.map { it.id }
        context.launchOrTell(pickFiles, arrayOf("*/*"), R.string.no_file_picker)
    }
    fun sendFolder(to: List<Contact>) {
        targets = to.map { it.id }
        context.launchOrTell(pickFolder, null, R.string.no_folder_picker)
    }
    val shareTitle = stringResource(R.string.share_id_title)
    fun copy(id: String) {
        scope.launch { clipboard.setClipEntry(ClipEntry(ClipData.newPlainText(id, id))) }
        // Android 13 and later say so themselves.
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) context.tell(R.string.id_copied)
    }

    Scaffold(
        topBar = {
            if (selecting) {
                SelectionTopBar(count = chosen.size, onClose = { selected = emptyList() }) {
                    Box {
                        IconButton(onClick = { sendOpen = true }) {
                            Icon(
                                Icons.AutoMirrored.Filled.Send,
                                contentDescription = stringResource(R.string.action_send),
                            )
                        }
                        DropdownMenu(expanded = sendOpen, onDismissRequest = { sendOpen = false }) {
                            DropdownMenuItem(
                                text = { Text(stringResource(R.string.action_send_files)) },
                                onClick = { sendOpen = false; sendFiles(chosen) },
                            )
                            DropdownMenuItem(
                                text = { Text(stringResource(R.string.action_send_folder)) },
                                onClick = { sendOpen = false; sendFolder(chosen) },
                            )
                        }
                    }
                }
            } else TopAppBar(
                title = { Text(stringResource(R.string.nav_recipients)) },
                actions = {
                    Box {
                        IconButton(onClick = { menuOpen = true }) {
                            Icon(Icons.Filled.MoreVert, contentDescription = stringResource(R.string.more_options))
                        }
                        DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
                            DropdownMenuItem(
                                text = { Text(stringResource(R.string.menu_share_my_id)) },
                                onClick = {
                                    menuOpen = false
                                    val send = Intent(Intent.ACTION_SEND).setType("text/plain")
                                        .putExtra(Intent.EXTRA_TEXT, state.myId)
                                    context.startOrTell(
                                        Intent.createChooser(send, shareTitle),
                                        R.string.no_sharing,
                                        what = "sharing the Contact ID",
                                    )
                                },
                                enabled = state.myId.isNotEmpty(),
                            )
                            DropdownMenuItem(
                                text = { Text(stringResource(R.string.menu_copy_my_id)) },
                                onClick = { menuOpen = false; copy(state.myId) },
                                enabled = state.myId.isNotEmpty(),
                            )
                            DropdownMenuItem(
                                text = { Text(stringResource(R.string.menu_settings)) },
                                onClick = { menuOpen = false; navigator.go(SettingsKey) },
                            )
                            DropdownMenuItem(
                                text = { Text(stringResource(R.string.menu_help)) },
                                onClick = { menuOpen = false; navigator.go(HelpKey) },
                            )
                            DropdownMenuItem(
                                text = { Text(stringResource(R.string.menu_about)) },
                                onClick = { menuOpen = false; navigator.go(AboutKey) },
                            )
                        }
                    }
                },
            )
        },
        floatingActionButton = {
            if (state.state is CoreState.Running && !selecting) {
                // Icon and text as content: the form with `icon` and `text`
                // keeps its text from accessibility services (Material 3 1.4).
                ExtendedFloatingActionButton(onClick = { navigator.go(RecipientEditorKey()) }) {
                    Icon(Icons.Filled.Add, contentDescription = null)
                    Spacer(Modifier.width(12.dp))
                    Text(stringResource(R.string.add_recipient))
                }
            }
        },
    ) { padding ->
        Column(Modifier.fillMaxSize().padding(padding)) {
            when (val core = state.state) {
                CoreState.Starting -> StatusLine(stringResource(R.string.starting))
                is CoreState.Failed -> {
                    Text(
                        stringResource(R.string.cannot_start),
                        style = MaterialTheme.typography.titleMedium,
                        modifier = Modifier.padding(16.dp).semantics { heading() },
                    )
                    Text(core.reason, modifier = Modifier.padding(horizontal = 16.dp))
                }
                is CoreState.Running -> {
                    StatusLine(state.line)
                    OfferBanner(offers, onReview = { navigator.incoming(it.id) })
                    if (state.recipients.isEmpty()) {
                        Text(
                            stringResource(R.string.recipients_empty),
                            modifier = Modifier.padding(16.dp),
                        )
                    }
                    LazyColumn(contentPadding = PaddingValues(bottom = 88.dp)) {
                        items(state.recipients, key = { it.contact.id }) { item ->
                            RecipientRow(
                                item = item,
                                selecting = selecting,
                                selected = item.contact.id in selected,
                                onToggle = { toggle(item.contact) },
                                onSendFiles = { sendFiles(listOf(item.contact)) },
                                onSendFolder = { sendFolder(listOf(item.contact)) },
                                onEdit = { navigator.go(RecipientEditorKey(item.contact.id)) },
                                onCopy = { copy(item.contact.id) },
                                onBlock = { pending = Pending.Block(item.contact) },
                                onRemove = { pending = Pending.Remove(item.contact) },
                            )
                            HorizontalDivider()
                        }
                    }
                }
            }
        }
    }

    when (val question = pending) {
        is Pending.Remove -> ConfirmDialog(
            title = stringResource(R.string.remove_confirm, question.contact.name),
            text = stringResource(R.string.remove_text),
            confirm = stringResource(R.string.remove),
            onConfirm = { pending = null; viewModel.remove(question.contact) },
            onDismiss = { pending = null },
        )
        is Pending.Block -> ConfirmDialog(
            title = stringResource(R.string.block_confirm, question.contact.name),
            text = stringResource(R.string.block_recipient_text),
            confirm = stringResource(R.string.block),
            onConfirm = { pending = null; viewModel.block(question.contact) },
            onDismiss = { pending = null },
        )
        null -> {}
    }
    state.message?.let { MessageDialog(it, onDismiss = viewModel::messageShown) }
}

/**
 * A recipient: a tap sends files; the other actions are in its menu, with
 * Select, which a long press does as well. While recipients are being
 * selected, a tap selects this one or takes it out.
 */
@Composable
private fun RecipientRow(
    item: RecipientItem,
    selecting: Boolean,
    selected: Boolean,
    onToggle: () -> Unit,
    onSendFiles: () -> Unit,
    onSendFolder: () -> Unit,
    onEdit: () -> Unit,
    onCopy: () -> Unit,
    onBlock: () -> Unit,
    onRemove: () -> Unit,
) {
    var open by remember { mutableStateOf(false) }
    val sendLabel = stringResource(R.string.action_send_files)
    RecipientListItem(
        name = item.contact.name,
        supporting = item.transfer ?: shortId(item.contact.id),
        selecting = selecting,
        selected = selected,
        onClick = onSendFiles,
        onToggle = onToggle,
        onClickLabel = sendLabel,
        trailing = {
            Box {
                IconButton(onClick = { open = true }) {
                    Icon(
                        Icons.Filled.MoreVert,
                        contentDescription = stringResource(R.string.recipient_options, item.contact.name),
                    )
                }
                DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
                    val choose = { action: () -> Unit -> { open = false; action() } }
                    DropdownMenuItem(text = { Text(sendLabel) }, onClick = choose(onSendFiles))
                    DropdownMenuItem(
                        text = { Text(stringResource(R.string.action_send_folder)) },
                        onClick = choose(onSendFolder),
                    )
                    DropdownMenuItem(
                        text = { Text(stringResource(R.string.action_select)) },
                        onClick = choose(onToggle),
                    )
                    DropdownMenuItem(text = { Text(stringResource(R.string.action_edit)) }, onClick = choose(onEdit))
                    DropdownMenuItem(
                        text = { Text(stringResource(R.string.action_copy_id)) },
                        onClick = choose(onCopy),
                    )
                    DropdownMenuItem(text = { Text(stringResource(R.string.action_block)) }, onClick = choose(onBlock))
                    DropdownMenuItem(
                        text = { Text(stringResource(R.string.action_remove)) },
                        onClick = choose(onRemove),
                    )
                }
            }
        },
    )
}
