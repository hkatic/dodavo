package com.hrvojekatic.dodavo.ui.recipients

import android.net.Uri
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.hrvojekatic.dodavo.R
import com.hrvojekatic.dodavo.ui.Navigator
import com.hrvojekatic.dodavo.ui.TransfersKey
import com.hrvojekatic.dodavo.ui.common.MessageDialog
import com.hrvojekatic.dodavo.ui.common.StatusLine
import com.hrvojekatic.dodavo.ui.common.container
import com.hrvojekatic.dodavo.ui.common.shortId
import kotlinx.coroutines.launch

/**
 * Items shared from another app: the user chooses who gets them. They are
 * copied at once, while this screen still may read them.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SendToScreen(uris: List<String>, navigator: Navigator) {
    val container = LocalContext.current.container
    val contacts by container.repository.contacts.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()
    var busy by remember { mutableStateOf(false) }
    var message by remember { mutableStateOf<String?>(null) }
    Scaffold(
        topBar = {
            TopAppBar(
                navigationIcon = {
                    IconButton(onClick = navigator::back) {
                        Icon(Icons.Filled.Close, contentDescription = stringResource(R.string.close))
                    }
                },
                title = { Text(stringResource(R.string.action_send_files)) },
            )
        },
    ) { padding ->
        Column(Modifier.fillMaxSize().padding(padding)) {
            if (contacts.isEmpty()) {
                StatusLine(stringResource(R.string.recipients_empty))
            }
            LazyColumn {
                items(contacts, key = { it.id }) { contact ->
                    ListItem(
                        headlineContent = { Text(contact.name) },
                        supportingContent = { Text(shortId(contact.id)) },
                        modifier = Modifier
                            .clickable(enabled = !busy) {
                                busy = true
                                scope.launch {
                                    val error = runCatching {
                                        container.sending.sendFiles(contact.id, uris.map(Uri::parse))
                                    }.getOrElse { it.message }
                                    busy = false
                                    if (error != null) message = error else navigator.top(TransfersKey)
                                }
                            }
                            .padding(end = 8.dp),
                    )
                    HorizontalDivider()
                }
            }
        }
    }
    message?.let { MessageDialog(it, onDismiss = { message = null }) }
}
