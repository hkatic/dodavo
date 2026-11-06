package com.hrvojekatic.dodavo

import android.content.Context
import android.os.Build
import android.os.Environment
import android.provider.Settings
import androidx.core.os.ConfigurationCompat
import com.hrvojekatic.dodavo.core.CoreRepository
import com.hrvojekatic.dodavo.core.NativeCore
import com.hrvojekatic.dodavo.core.Sending
import com.hrvojekatic.dodavo.core.StartOptions
import java.io.File
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob

/** What the app's parts share, made once (manual dependency injection). */
class AppContainer(private val context: Context) {
    /** Lives as long as the process. */
    val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    val repository = CoreRepository(NativeCore(context), scope)

    val sending = Sending(context, repository)

    /** The folders and preferences that only the app can ask the system for. */
    fun startOptions(): StartOptions {
        val locales = ConfigurationCompat.getLocales(context.resources.configuration)
        val languages = (0 until locales.size()).mapNotNull { locales[it]?.toLanguageTag() }
        return StartOptions(
            // Backed up, except for the identity and the transfers (see
            // res/xml/data_extraction_rules.xml).
            dataDir = File(context.filesDir, "Dodavo").path,
            // Not the cache, which the system may empty while a transfer waits.
            tempDir = File(context.noBackupFilesDir, "tmp").path,
            downloadsDir = File(
                Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS),
                "Dodavo",
            ).path,
            language = context.getString(R.string.dodavo_lang),
            languages = languages,
            deviceName = Settings.Global.getString(context.contentResolver, Settings.Global.DEVICE_NAME)
                ?.takeIf { it.isNotBlank() } ?: Build.MODEL,
        )
    }
}
