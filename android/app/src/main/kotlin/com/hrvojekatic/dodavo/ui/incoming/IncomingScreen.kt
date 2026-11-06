package com.hrvojekatic.dodavo.ui.incoming

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import com.hrvojekatic.dodavo.R
import com.hrvojekatic.dodavo.core.AcceptCheck
import com.hrvojekatic.dodavo.core.CoreRepository
import com.hrvojekatic.dodavo.core.Offer
import com.hrvojekatic.dodavo.ui.Navigator
import com.hrvojekatic.dodavo.ui.common.ConfirmDialog
import com.hrvojekatic.dodavo.ui.common.MessageDialog
import com.hrvojekatic.dodavo.ui.common.appViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch

/** What stands between the user and the answer. */
sealed interface Question {
    data class Problem(val text: String) : Question
    data class Confirm(val text: String) : Question
}

data class IncomingUiState(
    /** Null once it is answered, or the sender gave up. */
    val offer: Offer? = null,
    val question: Question? = null,
    /** Answered: the screen closes. */
    val done: Boolean = false,
)

class IncomingViewModel(private val repository: CoreRepository, private val id: Long) : ViewModel() {
    private val question = MutableStateFlow<Question?>(null)
    private val done = MutableStateFlow(false)

    val uiState: StateFlow<IncomingUiState> = combine(repository.offers, question, done) { offers, question, done ->
        IncomingUiState(offer = offers[id], question = question, done = done || offers[id] == null)
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), IncomingUiState(offer = repository.offers.value[id]))

    /** Accepts it, and adds the sender to the recipients if [addSender]. */
    fun accept(addSender: Boolean, anyway: Boolean = false) {
        val offer = repository.offers.value[id] ?: return
        viewModelScope.launch {
            if (!anyway) {
                when (val check = repository.checkAccept(id)) {
                    is AcceptCheck.Problem -> { question.value = Question.Problem(check.text); return@launch }
                    is AcceptCheck.Confirm -> { question.value = Question.Confirm(check.text); return@launch }
                    AcceptCheck.Fine -> {}
                }
            }
            question.value = null
            // A name that is taken is not worth losing the transfer for: the
            // sender stays unknown, and can be added afterwards.
            if (addSender && repository.recipientProblem(offer.fromName, offer.fromId) == null) {
                repository.addRecipient(offer.fromName, offer.fromId)
            }
            repository.accept(id)
            done.value = true
        }
    }

    fun decline() {
        viewModelScope.launch {
            repository.decline(id)
            done.value = true
        }
    }

    fun block() {
        val offer = repository.offers.value[id] ?: return
        viewModelScope.launch {
            repository.block(offer.fromName, offer.fromId)
            done.value = true
        }
    }

    fun dismissQuestion() {
        question.value = null
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun IncomingScreen(offerId: Long, navigator: Navigator) {
    val viewModel = appViewModel { IncomingViewModel(it.repository, offerId) }
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    var addSender by rememberSaveable { mutableStateOf(false) }
    var menuOpen by remember { mutableStateOf(false) }
    var blocking by remember { mutableStateOf(false) }
    LaunchedEffect(state.done) { if (state.done) navigator.back() }
    val offer = state.offer ?: return

    Scaffold(
        topBar = {
            TopAppBar(
                navigationIcon = {
                    // Back leaves it waiting; only Decline declines it.
                    IconButton(onClick = navigator::back) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = stringResource(R.string.navigate_up))
                    }
                },
                title = { Text(stringResource(R.string.incoming_title)) },
                actions = {
                    Box {
                        IconButton(onClick = { menuOpen = true }) {
                            Icon(Icons.Filled.MoreVert, contentDescription = stringResource(R.string.more_options))
                        }
                        DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
                            DropdownMenuItem(
                                text = { Text(stringResource(R.string.block_sender)) },
                                onClick = { menuOpen = false; blocking = true },
                            )
                        }
                    }
                },
            )
        },
    ) { padding ->
        Column(
            Modifier
                .fillMaxSize()
                .padding(padding)
                .padding(16.dp)
                .verticalScroll(rememberScrollState()),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(offer.headline, style = MaterialTheme.typography.headlineSmall, modifier = Modifier.semantics { heading() })
            Text(offer.description, style = MaterialTheme.typography.bodyLarge)
            val warning = offer.unknownWarning
            val addLabel = offer.addLabel
            if (warning != null) {
                Row(verticalAlignment = Alignment.Top) {
                    Icon(
                        Icons.Filled.Warning,
                        contentDescription = null,
                        tint = MaterialTheme.colorScheme.error,
                        modifier = Modifier.padding(end = 8.dp),
                    )
                    Text(warning)
                }
            }
            if (addLabel != null) {
                Row(
                    Modifier
                        .fillMaxWidth()
                        .toggleable(value = addSender, role = Role.Checkbox, onValueChange = { addSender = it }),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Checkbox(checked = addSender, onCheckedChange = null)
                    Text(addLabel, modifier = Modifier.padding(start = 8.dp))
                }
            }
            Text(stringResource(R.string.saved_in))
            Text(offer.space.text)
            Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                OutlinedButton(onClick = viewModel::decline) { Text(stringResource(R.string.decline)) }
                Button(onClick = { viewModel.accept(addSender) }) { Text(stringResource(R.string.accept)) }
            }
        }
    }

    when (val question = state.question) {
        is Question.Problem -> MessageDialog(question.text, onDismiss = viewModel::dismissQuestion)
        is Question.Confirm -> ConfirmDialog(
            title = question.text,
            text = null,
            confirm = stringResource(R.string.accept_anyway),
            onConfirm = { viewModel.accept(addSender, anyway = true) },
            onDismiss = viewModel::dismissQuestion,
        )
        null -> {}
    }
    if (blocking) {
        ConfirmDialog(
            title = stringResource(R.string.block_confirm, offer.fromName),
            text = stringResource(if (offer.known) R.string.block_recipient_text else R.string.block_text),
            confirm = stringResource(R.string.block),
            onConfirm = { blocking = false; viewModel.block() },
            onDismiss = { blocking = false },
        )
    }
}
