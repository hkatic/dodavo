package com.hrvojekatic.dodavo.ui.about

import android.content.ActivityNotFoundException
import android.content.ClipData
import android.content.Context
import android.content.Intent
import android.widget.Toast
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
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.core.content.FileProvider
import androidx.core.net.toUri
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.hrvojekatic.dodavo.R
import com.hrvojekatic.dodavo.core.CoreState
import com.hrvojekatic.dodavo.ui.LicensesKey
import com.hrvojekatic.dodavo.ui.Navigator
import com.hrvojekatic.dodavo.ui.common.container
import java.io.File
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** Where the privacy policy is published. */
const val PRIVACY_POLICY_URL = "https://github.com/hkatic/dodavo/blob/master/PRIVACY.md"

/**
 * Shares a copy of Dodavo's log through Android's share sheet, for the developers. The copy is in
 * a folder of its own in the cache, which is all of the app's own files that the file provider
 * names (`res/xml/file_paths.xml`).
 */
private suspend fun shareLog(context: Context) {
    val path = context.container.repository.flushLog()
    if (path.isEmpty()) return
    val copy = withContext(Dispatchers.IO) {
        runCatching {
            val folder = File(context.cacheDir, "log").apply { mkdirs() }
            File(path).copyTo(File(folder, "Dodavo.log"), overwrite = true)
        }.getOrNull()
    } ?: return
    val uri = FileProvider.getUriForFile(context, "${context.packageName}.files", copy)
    val intent = Intent(Intent.ACTION_SEND)
        .setType("text/plain")
        .putExtra(Intent.EXTRA_STREAM, uri)
        .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
    intent.clipData = ClipData.newRawUri("", uri)
    try {
        context.startActivity(Intent.createChooser(intent, context.getString(R.string.share_log)))
    } catch (_: ActivityNotFoundException) {
        Toast.makeText(context, R.string.no_app_to_open, Toast.LENGTH_LONG).show()
    }
}

/** The name, the version, and the notices that the GPL asks for. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AboutScreen(navigator: Navigator) {
    val context = LocalContext.current
    val state by context.container.repository.state.collectAsStateWithLifecycle()
    val started = (state as? CoreState.Running)?.started
    val scope = rememberCoroutineScope()
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
            started?.logPath?.takeIf { it.isNotEmpty() }?.let {
                OutlinedButton(onClick = { scope.launch { shareLog(context) } }) {
                    Text(stringResource(R.string.share_log))
                }
            }
        }
    }
}
