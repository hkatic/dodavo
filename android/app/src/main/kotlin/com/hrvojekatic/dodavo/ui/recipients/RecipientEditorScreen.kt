package com.hrvojekatic.dodavo.ui.recipients

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalClipboard
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.error
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import com.hrvojekatic.dodavo.R
import com.hrvojekatic.dodavo.core.CoreRepository
import com.hrvojekatic.dodavo.ui.Navigator
import com.hrvojekatic.dodavo.ui.common.appViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

data class EditorUiState(
    val name: String = "",
    val contactId: String = "",
    val nameError: String? = null,
    val idError: String? = null,
    /** Saved: the screen closes. */
    val done: Boolean = false,
)

/** Add a recipient, or edit the one with [editing]. */
class RecipientEditorViewModel(
    private val repository: CoreRepository,
    private val editing: String?,
) : ViewModel() {
    private val state = MutableStateFlow(
        repository.contacts.value.firstOrNull { it.id == editing }
            ?.let { EditorUiState(name = it.name, contactId = it.id) } ?: EditorUiState(),
    )
    val uiState: StateFlow<EditorUiState> = state.asStateFlow()

    fun name(text: String) = state.update { it.copy(name = text, nameError = null) }

    fun contactId(text: String) = state.update { it.copy(contactId = text, idError = null) }

    fun save() {
        val (name, id) = state.value.let { it.name to it.contactId }
        viewModelScope.launch {
            val problem = repository.recipientProblem(name, id, editing)
            if (problem != null) {
                state.update {
                    if (name.isBlank()) it.copy(nameError = problem) else it.copy(idError = problem)
                }
                return@launch
            }
            val error = if (editing == null) {
                repository.addRecipient(name, id)
            } else {
                repository.editRecipient(editing, name, id)
            }
            state.update { if (error != null) it.copy(idError = error) else it.copy(done = true) }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun RecipientEditorScreen(contactId: String?, navigator: Navigator) {
    val viewModel = appViewModel { RecipientEditorViewModel(it.repository, contactId) }
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    val clipboard = LocalClipboard.current
    val scope = rememberCoroutineScope()
    LaunchedEffect(state.done) { if (state.done) navigator.back() }

    Scaffold(
        topBar = {
            TopAppBar(
                navigationIcon = {
                    IconButton(onClick = navigator::back) {
                        Icon(Icons.Filled.Close, contentDescription = stringResource(R.string.close))
                    }
                },
                title = {
                    Text(stringResource(if (contactId == null) R.string.add_recipient else R.string.edit_recipient))
                },
                actions = {
                    TextButton(onClick = viewModel::save) { Text(stringResource(R.string.save)) }
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
        ) {
            Field(
                value = state.name,
                onValue = viewModel::name,
                label = stringResource(R.string.recipient_name_label),
                help = stringResource(R.string.recipient_name_help),
                error = state.nameError,
                keyboard = KeyboardOptions(capitalization = KeyboardCapitalization.Words, imeAction = ImeAction.Next),
            )
            Field(
                value = state.contactId,
                onValue = viewModel::contactId,
                label = stringResource(R.string.contact_id_label),
                help = stringResource(R.string.contact_id_help),
                error = state.idError,
                keyboard = KeyboardOptions(
                    keyboardType = KeyboardType.Ascii,
                    autoCorrectEnabled = false,
                    imeAction = ImeAction.Done,
                ),
                onDone = viewModel::save,
            )
            TextButton(
                onClick = {
                    scope.launch {
                        clipboard.getClipEntry()?.clipData?.takeIf { it.itemCount > 0 }
                            ?.getItemAt(0)?.text?.toString()?.trim()?.let(viewModel::contactId)
                    }
                },
            ) { Text(stringResource(R.string.paste)) }
        }
    }
}

/** A text field with its label, a line of help, and the error in its place. */
@Composable
private fun Field(
    value: String,
    onValue: (String) -> Unit,
    label: String,
    help: String,
    error: String?,
    keyboard: KeyboardOptions,
    onDone: () -> Unit = {},
) {
    OutlinedTextField(
        value = value,
        onValueChange = onValue,
        label = { Text(label) },
        supportingText = { Text(error ?: help) },
        isError = error != null,
        singleLine = true,
        keyboardOptions = keyboard,
        keyboardActions = KeyboardActions(onDone = { onDone() }),
        modifier = Modifier
            .fillMaxWidth()
            .padding(bottom = 8.dp)
            .semantics { if (error != null) error(error) },
    )
}
