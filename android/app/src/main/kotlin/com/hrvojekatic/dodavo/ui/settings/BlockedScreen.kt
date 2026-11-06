package com.hrvojekatic.dodavo.ui.settings

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import com.hrvojekatic.dodavo.R
import com.hrvojekatic.dodavo.core.CoreRepository
import com.hrvojekatic.dodavo.ui.Navigator
import com.hrvojekatic.dodavo.ui.common.StatusLine
import com.hrvojekatic.dodavo.ui.common.appViewModel
import com.hrvojekatic.dodavo.ui.common.shortId
import kotlinx.coroutines.launch

class BlockedViewModel(private val repository: CoreRepository) : ViewModel() {
    val blocked = repository.blocked

    fun unblock(id: String) = viewModelScope.launch { repository.unblock(id) }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun BlockedScreen(navigator: Navigator) {
    val viewModel = appViewModel { BlockedViewModel(it.repository) }
    val blocked by viewModel.blocked.collectAsStateWithLifecycle()
    Scaffold(
        topBar = {
            TopAppBar(
                navigationIcon = {
                    IconButton(onClick = navigator::back) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = stringResource(R.string.navigate_up))
                    }
                },
                title = { Text(stringResource(R.string.blocked_senders)) },
            )
        },
    ) { padding ->
        Column(Modifier.fillMaxSize().padding(padding)) {
            if (blocked.isEmpty()) {
                // Said aloud also when the last one is unblocked.
                StatusLine(stringResource(R.string.blocked_none))
            }
            LazyColumn {
                items(blocked, key = { it.id }) { sender ->
                    val label = stringResource(R.string.unblock_named, sender.name)
                    ListItem(
                        headlineContent = { Text(sender.name) },
                        supportingContent = { Text(shortId(sender.id)) },
                        trailingContent = {
                            TextButton(
                                onClick = { viewModel.unblock(sender.id) },
                                modifier = Modifier.semantics { contentDescription = label },
                            ) { Text(stringResource(R.string.unblock)) }
                        },
                        modifier = Modifier.padding(end = 8.dp),
                    )
                    HorizontalDivider()
                }
            }
        }
    }
}
