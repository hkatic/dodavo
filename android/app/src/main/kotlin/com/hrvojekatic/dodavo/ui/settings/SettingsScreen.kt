package com.hrvojekatic.dodavo.ui.settings

import android.Manifest
import android.content.ClipData
import android.content.pm.PackageManager
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.core.content.ContextCompat
import com.hrvojekatic.dodavo.AppPreferences
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.provider.Settings as SystemSettings
import androidx.appcompat.app.AppCompatDelegate
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.platform.ClipEntry
import androidx.compose.ui.platform.LocalClipboard
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.dp
import androidx.core.os.LocaleListCompat
import androidx.lifecycle.ViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import com.hrvojekatic.dodavo.R
import com.hrvojekatic.dodavo.core.CoreRepository
import com.hrvojekatic.dodavo.core.CoreState
import com.hrvojekatic.dodavo.core.ReceivedFolder
import com.hrvojekatic.dodavo.core.Settings
import com.hrvojekatic.dodavo.ui.BlockedKey
import com.hrvojekatic.dodavo.ui.Navigator
import com.hrvojekatic.dodavo.ui.common.MessageDialog
import com.hrvojekatic.dodavo.ui.common.appViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch

data class SettingsUiState(
    val settings: Settings? = null,
    val receiveInBackground: Boolean = false,
    val myId: String = "",
    /** Each language by its code, with its own name for it. */
    val languages: List<Pair<String, String>> = emptyList(),
    val message: String? = null,
)

class SettingsViewModel(
    private val repository: CoreRepository,
    private val preferences: AppPreferences,
) : ViewModel() {
    private val message = MutableStateFlow<String?>(null)

    val uiState: StateFlow<SettingsUiState> = combine(
        repository.settings,
        repository.state,
        preferences.receiveInBackground,
        message,
    ) { settings, state, background, message ->
        val started = (state as? CoreState.Running)?.started
        SettingsUiState(
            settings = settings,
            receiveInBackground = background,
            myId = started?.myId.orEmpty(),
            languages = started?.languages.orEmpty().filter { it.size == 2 }.map { it[0] to it[1] },
            message = message,
        )
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), SettingsUiState())

    private fun change(edit: (Settings) -> Settings) {
        val current = repository.settings.value ?: return
        val next = edit(current)
        if (next == current) return
        viewModelScope.launch {
            message.value = repository.settingsProblem(next) ?: repository.setSettings(next)
        }
    }

    fun name(name: String) = change { it.copy(myName = name.trim()) }
    fun autoAccept(on: Boolean) = change { it.copy(autoAccept = on) }
    fun receivedFolder(folder: ReceivedFolder) = change { it.copy(receivedFolder = folder) }
    fun receiveInBackground(on: Boolean) {
        viewModelScope.launch { preferences.setReceiveInBackground(on) }
    }

    fun messageShown() { message.value = null }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsScreen(navigator: Navigator) {
    val viewModel = appViewModel { SettingsViewModel(it.repository, it.preferences) }
    // Offers arrive as notifications while Dodavo is closed.
    val notifications = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) {
        viewModel.receiveInBackground(true)
    }
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    val context = LocalContext.current
    val clipboard = LocalClipboard.current
    val scope = rememberCoroutineScope()
    var choosingLanguage by remember { mutableStateOf(false) }
    val settings = state.settings

    Scaffold(
        topBar = {
            TopAppBar(
                navigationIcon = {
                    IconButton(onClick = navigator::back) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = stringResource(R.string.navigate_up))
                    }
                },
                title = { Text(stringResource(R.string.settings_title)) },
            )
        },
    ) { padding ->
        Column(
            Modifier
                .fillMaxSize()
                .padding(padding)
                .verticalScroll(rememberScrollState()),
        ) {
            if (settings != null) {
                NameField(settings.myName, onCommit = viewModel::name)
            }
            Heading(stringResource(R.string.my_id_label))
            SelectionContainer(Modifier.padding(horizontal = 16.dp)) {
                Text(state.myId, style = MaterialTheme.typography.bodyMedium)
            }
            Row(Modifier.padding(horizontal = 16.dp, vertical = 8.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedButton(onClick = {
                    scope.launch { clipboard.setClipEntry(ClipEntry(ClipData.newPlainText(state.myId, state.myId))) }
                }) { Text(stringResource(R.string.action_copy_id)) }
                val title = stringResource(R.string.share_id_title)
                OutlinedButton(onClick = {
                    val send = Intent(Intent.ACTION_SEND).setType("text/plain").putExtra(Intent.EXTRA_TEXT, state.myId)
                    context.startActivity(Intent.createChooser(send, title))
                }) { Text(stringResource(R.string.share)) }
            }
            HorizontalDivider()
            if (settings != null) {
                SwitchRow(
                    title = stringResource(R.string.receive_background),
                    help = stringResource(R.string.receive_background_help),
                    checked = state.receiveInBackground,
                    onChange = { on ->
                        val ask = on && Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
                            ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) !=
                            PackageManager.PERMISSION_GRANTED
                        if (ask) notifications.launch(Manifest.permission.POST_NOTIFICATIONS) else viewModel.receiveInBackground(on)
                    },
                )
                SwitchRow(
                    title = stringResource(R.string.auto_accept),
                    help = stringResource(R.string.auto_accept_help),
                    checked = settings.autoAccept,
                    onChange = viewModel::autoAccept,
                )
                Heading(stringResource(R.string.received_folder_label))
                Column(Modifier.selectableGroup()) {
                    for ((folder, label) in listOf(
                        ReceivedFolder.Extract to R.string.received_extract,
                        ReceivedFolder.KeepZip to R.string.received_zip,
                        ReceivedFolder.Both to R.string.received_both,
                    )) {
                        Row(
                            Modifier
                                .fillMaxWidth()
                                .selectable(
                                    selected = settings.receivedFolder == folder,
                                    role = Role.RadioButton,
                                    onClick = { viewModel.receivedFolder(folder) },
                                )
                                .padding(horizontal = 16.dp, vertical = 12.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            RadioButton(selected = settings.receivedFolder == folder, onClick = null)
                            Text(stringResource(label), modifier = Modifier.padding(start = 16.dp))
                        }
                    }
                }
                HorizontalDivider()
            }
            LinkRow(stringResource(R.string.blocked_senders), null) { navigator.go(BlockedKey) }
            LinkRow(stringResource(R.string.language_setting), stringResource(R.string.language_setting_help)) {
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                    context.startActivity(
                        Intent(SystemSettings.ACTION_APP_LOCALE_SETTINGS, Uri.fromParts("package", context.packageName, null)),
                    )
                } else {
                    choosingLanguage = true
                }
            }
            LinkRow(stringResource(R.string.notifications_setting), stringResource(R.string.notifications_setting_help)) {
                context.startActivity(
                    Intent(SystemSettings.ACTION_APP_NOTIFICATION_SETTINGS)
                        .putExtra(SystemSettings.EXTRA_APP_PACKAGE, context.packageName),
                )
            }
        }
    }

    if (choosingLanguage) {
        LanguageDialog(state.languages, onDismiss = { choosingLanguage = false })
    }
    state.message?.let { MessageDialog(it, onDismiss = viewModel::messageShown) }
}

@Composable
private fun Heading(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.titleSmall,
        color = MaterialTheme.colorScheme.primary,
        modifier = Modifier
            .padding(start = 16.dp, end = 16.dp, top = 16.dp, bottom = 4.dp)
            .semantics { heading() },
    )
}

/** The name, saved when the field is left or Done is pressed. */
@Composable
private fun NameField(saved: String, onCommit: (String) -> Unit) {
    var text by remember(saved) { mutableStateOf(saved) }
    OutlinedTextField(
        value = text,
        onValueChange = { text = it },
        label = { Text(stringResource(R.string.my_name_label)) },
        supportingText = { Text(stringResource(R.string.my_name_help)) },
        singleLine = true,
        keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done),
        keyboardActions = KeyboardActions(onDone = { onCommit(text) }),
        modifier = Modifier
            .fillMaxWidth()
            .padding(16.dp)
            .onFocusChanged { if (!it.isFocused && text != saved) onCommit(text) },
    )
}

/** A switch and what it is about, as one control. */
@Composable
fun SwitchRow(title: String, help: String?, checked: Boolean, onChange: (Boolean) -> Unit, enabled: Boolean = true) {
    ListItem(
        headlineContent = { Text(title) },
        supportingContent = help?.let { { Text(it) } },
        trailingContent = { Switch(checked = checked, onCheckedChange = null, enabled = enabled) },
        modifier = Modifier.toggleable(value = checked, enabled = enabled, role = Role.Switch, onValueChange = onChange),
    )
}

@Composable
private fun LinkRow(title: String, help: String?, onClick: () -> Unit) {
    ListItem(
        headlineContent = { Text(title) },
        supportingContent = help?.let { { Text(it) } },
        modifier = Modifier.selectable(selected = false, role = Role.Button, onClick = onClick),
    )
}

/** Android 12 and earlier: the app's language, from its own list. */
@Composable
private fun LanguageDialog(languages: List<Pair<String, String>>, onDismiss: () -> Unit) {
    val current = AppCompatDelegate.getApplicationLocales().toLanguageTags()
    val choices = listOf("" to stringResource(R.string.language_system)) + languages
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.language_setting)) },
        text = {
            Column(Modifier.selectableGroup()) {
                for ((code, name) in choices) {
                    val selected = current.equals(code, ignoreCase = true) ||
                        (code.isEmpty() && current.isEmpty())
                    Row(
                        Modifier
                            .fillMaxWidth()
                            .selectable(selected = selected, role = Role.RadioButton, onClick = {
                                AppCompatDelegate.setApplicationLocales(LocaleListCompat.forLanguageTags(code))
                                onDismiss()
                            })
                            .padding(vertical = 12.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        RadioButton(selected = selected, onClick = null)
                        Text(name, modifier = Modifier.padding(start = 16.dp))
                    }
                }
            }
        },
        confirmButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.cancel)) } },
    )
}
