package com.hrvojekatic.dodavo.ui.common

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.widget.Toast
import androidx.activity.result.ActivityResultLauncher
import androidx.annotation.StringRes
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.error
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider.AndroidViewModelFactory.Companion.APPLICATION_KEY
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import com.hrvojekatic.dodavo.AppContainer
import com.hrvojekatic.dodavo.DodavoApp
import com.hrvojekatic.dodavo.R
import kotlinx.coroutines.launch

/** The app's container, from any context. */
val Context.container: AppContainer
    get() = (applicationContext as DodavoApp).container

/** A ViewModel made from the app's container (manual dependency injection). */
@Composable
inline fun <reified VM : ViewModel> appViewModel(crossinline create: (AppContainer) -> VM): VM =
    viewModel(
        factory = viewModelFactory {
            initializer { create((this[APPLICATION_KEY] as DodavoApp).container) }
        },
    )

/**
 * Starts [intent]; false when nothing on the device takes it (no browser in a
 * work profile, a system screen that the maker left out), or when Android does
 * not let Dodavo start what takes it (a screen that its maker keeps to itself).
 */
fun Context.tryStart(intent: Intent): Boolean = started(intent.action) { startActivity(intent) }

/** Starts [intent], or says [unavailable]: by default, that no app can open the item. */
fun Context.startOrTell(intent: Intent, @StringRes unavailable: Int = R.string.no_app_to_open) {
    if (!tryStart(intent)) tell(unavailable)
}

/**
 * Launches [launcher] with [input], or says [unavailable]. [action] is what
 * the launcher's intent asks for, for the log.
 */
fun <I> Context.launchOrTell(launcher: ActivityResultLauncher<I>, input: I, action: String, @StringRes unavailable: Int) {
    if (!started(action) { launcher.launch(input) }) tell(unavailable)
}

/** Says [text] in a toast, which TalkBack reads. */
fun Context.tell(@StringRes text: Int) {
    Toast.makeText(this, text, Toast.LENGTH_LONG).show()
}

/** Whether [start] started an activity for [action]; what kept it from that goes into the log. */
private inline fun Context.started(action: String?, start: () -> Unit): Boolean {
    val why = try {
        start()
        return true
    } catch (e: ActivityNotFoundException) {
        e
    } catch (e: SecurityException) {
        e
    }
    // VIEW, OPEN_DOCUMENT: the log's scrubber takes a whole action for the name of a host.
    val what = action?.substringAfterLast('.') ?: "a named activity"
    val container = container
    container.scope.launch { container.repository.log("warn", "nothing started for $what (${why.javaClass.simpleName})") }
    return false
}

/** A Contact ID as people read it: `3f8a9c1e…e41b07`. */
fun shortId(id: String): String = if (id.length <= 16) id else "${id.take(8)}…${id.takeLast(6)}"

/**
 * A line that says what changes without moving the focus: what the core
 * announces, or else the online status. TalkBack reads it when it changes
 * (a polite live region), which Android prefers to announcements since 16.
 */
@Composable
fun StatusLine(text: String, modifier: Modifier = Modifier) {
    Text(
        text = text,
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 8.dp)
            .semantics { liveRegion = LiveRegionMode.Polite },
    )
}

/**
 * The size of a limit: a whole [number] and its [unit] ("mb" or "gb"). The
 * field takes digits only, and [onNumber] gets every change; [onNumberLeft]
 * is called when the focus leaves the field, and [onDone] for Done on the
 * keyboard. [error] takes the place under the field and is said with it.
 */
@Composable
fun LimitSizeFields(
    number: String,
    unit: String,
    onNumber: (String) -> Unit,
    onUnit: (String) -> Unit,
    modifier: Modifier = Modifier,
    error: String? = null,
    onNumberLeft: () -> Unit = {},
    onDone: () -> Unit = {},
) {
    Column(modifier) {
        OutlinedTextField(
            value = number,
            onValueChange = { text -> onNumber(text.filter(Char::isDigit).take(7)) },
            label = { Text(stringResource(R.string.limit_number_label)) },
            supportingText = error?.let { { Text(it) } },
            isError = error != null,
            singleLine = true,
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number, imeAction = ImeAction.Done),
            keyboardActions = KeyboardActions(onDone = { onDone() }),
            modifier = Modifier
                .fillMaxWidth()
                .onFocusChanged { if (!it.isFocused) onNumberLeft() }
                .semantics { if (error != null) error(error) },
        )
        Row(Modifier.selectableGroup()) {
            for ((code, label) in listOf("mb" to R.string.unit_mb, "gb" to R.string.unit_gb)) {
                Row(
                    Modifier
                        .selectable(selected = unit == code, role = Role.RadioButton, onClick = { onUnit(code) })
                        .padding(end = 32.dp, top = 12.dp, bottom = 12.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    RadioButton(selected = unit == code, onClick = null)
                    Text(stringResource(label), modifier = Modifier.padding(start = 16.dp))
                }
            }
        }
    }
}

/** A question with a button that does it and one that does not. */
@Composable
fun ConfirmDialog(
    title: String,
    text: String?,
    confirm: String,
    onConfirm: () -> Unit,
    onDismiss: () -> Unit,
    dismiss: String = stringResource(R.string.cancel),
) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(title) },
        text = text?.let { { Text(it) } },
        confirmButton = { TextButton(onClick = onConfirm) { Text(confirm) } },
        dismissButton = { TextButton(onClick = onDismiss) { Text(dismiss) } },
    )
}

/** A message with OK. */
@Composable
fun MessageDialog(text: String, onDismiss: () -> Unit) {
    AlertDialog(
        onDismissRequest = onDismiss,
        text = { Text(text) },
        confirmButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.ok)) } },
    )
}
