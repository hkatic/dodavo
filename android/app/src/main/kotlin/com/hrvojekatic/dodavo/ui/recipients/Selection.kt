package com.hrvojekatic.dodavo.ui.recipients

import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.selection.toggleable
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.Checkbox
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import com.hrvojekatic.dodavo.R

/**
 * The top bar while recipients are being selected: how many are, which
 * TalkBack reads when it changes (a polite live region), what can be done
 * with them, and the way out.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SelectionTopBar(count: Int, onClose: () -> Unit, actions: @Composable RowScope.() -> Unit) {
    TopAppBar(
        navigationIcon = {
            IconButton(onClick = onClose) {
                Icon(Icons.Filled.Close, contentDescription = stringResource(R.string.clear_selection))
            }
        },
        title = {
            Text(
                stringResource(R.string.selected_count, count),
                modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
            )
        },
        actions = actions,
    )
}

/**
 * A recipient in a list where several can be selected. Outside the selection
 * a tap does [onClick] and a long press starts the selection with this row,
 * which accessibility services offer as the action Select. While
 * [selecting], the row is a check box that a tap checks or clears, and its
 * [trailing] content is away.
 */
@Composable
fun RecipientListItem(
    name: String,
    supporting: String,
    selecting: Boolean,
    selected: Boolean,
    onClick: () -> Unit,
    onToggle: () -> Unit,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
    onClickLabel: String? = null,
    trailing: (@Composable () -> Unit)? = null,
) {
    val haptics = LocalHapticFeedback.current
    val select = stringResource(R.string.action_select)
    val leading: (@Composable () -> Unit)? = if (selecting) {
        { Checkbox(checked = selected, onCheckedChange = null) }
    } else {
        null
    }
    ListItem(
        headlineContent = { Text(name) },
        supportingContent = { Text(supporting) },
        leadingContent = leading,
        trailingContent = if (selecting) null else trailing,
        colors = if (selecting && selected) {
            ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.secondaryContainer)
        } else {
            ListItemDefaults.colors()
        },
        modifier = if (selecting) {
            modifier.toggleable(
                value = selected,
                enabled = enabled,
                role = Role.Checkbox,
                onValueChange = { onToggle() },
            )
        } else {
            modifier.combinedClickable(
                enabled = enabled,
                onClickLabel = onClickLabel,
                onLongClickLabel = select,
                onLongClick = {
                    haptics.performHapticFeedback(HapticFeedbackType.LongPress)
                    onToggle()
                },
                onClick = onClick,
            )
        },
    )
}
