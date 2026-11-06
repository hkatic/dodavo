package com.hrvojekatic.dodavo.ui.about

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.hrvojekatic.dodavo.R
import com.hrvojekatic.dodavo.ui.Navigator
import com.hrvojekatic.dodavo.ui.common.container

/** Dodavo's license, or those of its components, by paragraphs to read one by one. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun LicensesScreen(navigator: Navigator) {
    val repository = LocalContext.current.container.repository
    var texts by remember { mutableStateOf<Pair<String, String>?>(null) }
    var components by rememberSaveable { mutableStateOf(false) }
    LaunchedEffect(Unit) { texts = repository.licenses() }
    val paragraphs = remember(texts, components) {
        val text = texts?.let { if (components) it.second else it.first }.orEmpty()
        text.split(Regex("\\n\\s*\\n")).map(String::trim).filter(String::isNotEmpty)
    }
    Scaffold(
        topBar = {
            TopAppBar(
                navigationIcon = {
                    IconButton(onClick = navigator::back) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = stringResource(R.string.navigate_up))
                    }
                },
                title = { Text(stringResource(R.string.licenses)) },
            )
        },
    ) { padding ->
        Column(Modifier.fillMaxSize().padding(padding)) {
            SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth().padding(16.dp)) {
                val labels = listOf(R.string.license_dodavo, R.string.license_components)
                labels.forEachIndexed { index, label ->
                    SegmentedButton(
                        selected = components == (index == 1),
                        onClick = { components = index == 1 },
                        shape = SegmentedButtonDefaults.itemShape(index, labels.size),
                    ) { Text(stringResource(label)) }
                }
            }
            LazyColumn(Modifier.padding(horizontal = 16.dp)) {
                items(paragraphs) { paragraph ->
                    Text(paragraph, style = MaterialTheme.typography.bodySmall, modifier = Modifier.padding(bottom = 12.dp))
                }
            }
        }
    }
}
