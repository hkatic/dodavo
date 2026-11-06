package com.hrvojekatic.dodavo.ui.about

import android.content.Intent
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.core.net.toUri
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.hrvojekatic.dodavo.R
import com.hrvojekatic.dodavo.core.CoreState
import com.hrvojekatic.dodavo.ui.LicensesKey
import com.hrvojekatic.dodavo.ui.Navigator
import com.hrvojekatic.dodavo.ui.common.container

/** Where the privacy policy is published. */
const val PRIVACY_POLICY_URL = "https://github.com/hkatic/dodavo/blob/master/PRIVACY.md"

/** The name, the version, and the notices that the GPL asks for. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AboutScreen(navigator: Navigator) {
    val context = LocalContext.current
    val state by context.container.repository.state.collectAsStateWithLifecycle()
    val started = (state as? CoreState.Running)?.started
    fun browse(url: String) = context.startActivity(Intent(Intent.ACTION_VIEW, url.toUri()))
    Scaffold(
        topBar = {
            TopAppBar(
                navigationIcon = {
                    IconButton(onClick = navigator::back) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = stringResource(R.string.navigate_up))
                    }
                },
                title = { Text(stringResource(R.string.about_title)) },
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
            Text(stringResource(R.string.app_name), style = MaterialTheme.typography.headlineMedium, modifier = Modifier.semantics { heading() })
            started?.let {
                Text(stringResource(R.string.version, it.version))
                Text(stringResource(R.string.copyright_line, it.copyright, it.author))
            }
            Text(stringResource(R.string.about_text))
            Text(stringResource(R.string.about_notice))
            started?.website?.takeIf { it.isNotEmpty() }?.let { website ->
                OutlinedButton(onClick = { browse(website) }) { Text(stringResource(R.string.website)) }
            }
            OutlinedButton(onClick = { browse(PRIVACY_POLICY_URL) }) { Text(stringResource(R.string.privacy_policy)) }
            OutlinedButton(onClick = { navigator.go(LicensesKey) }) { Text(stringResource(R.string.licenses)) }
        }
    }
}
