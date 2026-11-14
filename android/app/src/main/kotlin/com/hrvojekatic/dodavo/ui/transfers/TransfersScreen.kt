package com.hrvojekatic.dodavo.ui.transfers

import android.app.DownloadManager
import android.content.Context
import android.content.Intent
import android.os.Environment
import android.webkit.MimeTypeMap
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material3.Card
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.core.content.FileProvider
import androidx.lifecycle.ViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import com.hrvojekatic.dodavo.R
import com.hrvojekatic.dodavo.core.CoreRepository
import com.hrvojekatic.dodavo.core.Row as TransferRow
import com.hrvojekatic.dodavo.ui.Navigator
import com.hrvojekatic.dodavo.ui.OfferBanner
import com.hrvojekatic.dodavo.ui.common.ConfirmDialog
import com.hrvojekatic.dodavo.ui.common.StatusLine
import com.hrvojekatic.dodavo.ui.common.appViewModel
import com.hrvojekatic.dodavo.ui.common.startOrTell
import com.hrvojekatic.dodavo.ui.waitingOffers
import java.io.File
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch

/** A transfer, and whether what it saved is a folder. */
data class TransferItem(val row: TransferRow, val folder: Boolean)

data class TransfersUiState(val line: String = "", val transfers: List<TransferItem> = emptyList())

class TransfersViewModel(private val repository: CoreRepository) : ViewModel() {
    val uiState: StateFlow<TransfersUiState> = combine(repository.line, repository.transfers) { line, rows ->
        TransfersUiState(
            line = line,
            transfers = rows.values.sortedByDescending { it.id }.map { row ->
                TransferItem(row, row.path?.let { File(it).isDirectory } ?: false)
            },
        )
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), TransfersUiState())

    fun pause(id: Long) = viewModelScope.launch { repository.pause(id) }
    fun resume(id: Long) = viewModelScope.launch { repository.resume(id) }
    fun cancel(id: Long) = viewModelScope.launch { repository.cancel(id) }
    fun remove(id: Long) = viewModelScope.launch { repository.removeRow(id) }
    fun clearFinished() = viewModelScope.launch { repository.clearFinished() }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun TransfersScreen(navigator: Navigator) {
    val viewModel = appViewModel { TransfersViewModel(it.repository) }
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    val offers = waitingOffers()
    var cancelling by remember { mutableStateOf<TransferRow?>(null) }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.nav_transfers)) },
                actions = {
                    if (state.transfers.any { !it.row.active }) {
                        TextButton(onClick = { viewModel.clearFinished() }) {
                            Text(stringResource(R.string.clear_finished))
                        }
                    }
                },
            )
        },
    ) { padding ->
        Column(Modifier.fillMaxSize().padding(padding)) {
            StatusLine(state.line)
            OfferBanner(offers, onReview = { navigator.incoming(it.id) })
            if (state.transfers.isEmpty()) {
                Text(stringResource(R.string.transfers_empty), modifier = Modifier.padding(16.dp))
            }
            LazyColumn(
                contentPadding = PaddingValues(16.dp),
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                items(state.transfers, key = { it.row.id }) { item ->
                    TransferCard(
                        item = item,
                        onPause = { viewModel.pause(item.row.id) },
                        onResume = { viewModel.resume(item.row.id) },
                        onCancel = { cancelling = item.row },
                        onRemove = { viewModel.remove(item.row.id) },
                    )
                }
            }
        }
    }

    cancelling?.let { row ->
        ConfirmDialog(
            title = stringResource(R.string.cancel_confirm, row.item),
            text = null,
            confirm = stringResource(R.string.cancel_transfer),
            dismiss = stringResource(R.string.keep_transferring),
            onConfirm = { cancelling = null; viewModel.cancel(row.id) },
            onDismiss = { cancelling = null },
        )
    }
}

@Composable
private fun TransferCard(
    item: TransferItem,
    onPause: () -> Unit,
    onResume: () -> Unit,
    onCancel: () -> Unit,
    onRemove: () -> Unit,
) {
    val row = item.row
    val context = LocalContext.current
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp)) {
            // What it is and how it goes, read as one.
            Column(Modifier.semantics(mergeDescendants = true) {}) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    if (!row.active) {
                        Icon(
                            if (row.succeeded) Icons.Filled.CheckCircle else Icons.Filled.Warning,
                            contentDescription = stringResource(
                                if (row.succeeded) R.string.succeeded_marker else R.string.failed_marker,
                            ),
                            tint = if (row.succeeded) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.error,
                            modifier = Modifier.padding(end = 8.dp),
                        )
                    }
                    Text(row.item, style = MaterialTheme.typography.titleMedium)
                }
                Text(
                    stringResource(if (row.sending) R.string.sending_to else R.string.receiving_from, row.peer),
                    style = MaterialTheme.typography.bodyMedium,
                )
                Text(row.status, style = MaterialTheme.typography.bodyMedium)
                listOf(row.amount, row.speed, row.remaining).filter { it.isNotEmpty() }
                    .takeIf { it.isNotEmpty() }
                    ?.let { Text(it.joinToString(" – "), style = MaterialTheme.typography.bodySmall) }
                if (row.detail.isNotEmpty()) {
                    Text(shownPaths(row.detail), style = MaterialTheme.typography.bodySmall)
                }
            }
            if (row.active) {
                val fraction = row.fraction
                val bar = Modifier.fillMaxWidth().padding(vertical = 8.dp)
                if (fraction != null) {
                    LinearProgressIndicator(progress = { fraction.toFloat() }, modifier = bar)
                } else {
                    LinearProgressIndicator(modifier = bar)
                }
            }
            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                if (row.active) {
                    if (row.canPause) {
                        OutlinedButton(onClick = if (row.paused) onResume else onPause) {
                            Text(stringResource(if (row.paused) R.string.resume else R.string.pause))
                        }
                    }
                    if (row.canCancel) {
                        OutlinedButton(onClick = onCancel) { Text(stringResource(R.string.cancel_transfer)) }
                    }
                } else {
                    val path = row.path
                    if (row.succeeded && path != null && !row.sending) {
                        if (item.folder) {
                            OutlinedButton(onClick = { showDownloads(context) }) {
                                Text(stringResource(R.string.show_folder))
                            }
                        } else {
                            OutlinedButton(onClick = { open(context, File(path)) }) {
                                Text(stringResource(R.string.open))
                            }
                            OutlinedButton(onClick = { share(context, File(path)) }) {
                                Text(stringResource(R.string.share))
                            }
                        }
                    }
                    TextButton(onClick = onRemove) { Text(stringResource(R.string.remove_from_list)) }
                }
            }
        }
    }
}

/** [text] with paths in the shared storage as Android shows them: "Download/Dodavo/a.pdf". */
private fun shownPaths(text: String): String =
    text.replace(sharedStorage + "/", "")

/** Where the shared storage is: what holds Download (see AppContainer). */
private val sharedStorage: String by lazy {
    Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS).parent.orEmpty()
}

/** The content URI of a received file, for other apps (res/xml/file_paths.xml). */
private fun uriOf(context: Context, file: File) =
    FileProvider.getUriForFile(context, "${context.packageName}.files", file)

private fun typeOf(file: File): String =
    MimeTypeMap.getSingleton().getMimeTypeFromExtension(file.extension.lowercase()) ?: "application/octet-stream"

private fun open(context: Context, file: File) {
    val intent = Intent(Intent.ACTION_VIEW)
        .setDataAndType(uriOf(context, file), typeOf(file))
        .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
    context.startOrTell(intent)
}

private fun share(context: Context, file: File) {
    val intent = Intent(Intent.ACTION_SEND)
        .setType(typeOf(file))
        .putExtra(Intent.EXTRA_STREAM, uriOf(context, file))
        .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
    context.startOrTell(Intent.createChooser(intent, file.name), R.string.no_sharing)
}

/** The system's own view of downloads, where Download/Dodavo is. */
private fun showDownloads(context: Context) {
    context.startOrTell(
        Intent(DownloadManager.ACTION_VIEW_DOWNLOADS).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
        R.string.no_app_for_folder,
    )
}
