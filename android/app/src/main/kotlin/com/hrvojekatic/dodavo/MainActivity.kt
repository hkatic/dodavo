package com.hrvojekatic.dodavo

import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Bundle
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.appcompat.app.AppCompatActivity
import androidx.core.content.IntentCompat
import androidx.lifecycle.lifecycleScope
import com.hrvojekatic.dodavo.ui.DodavoNav
import com.hrvojekatic.dodavo.ui.Request
import com.hrvojekatic.dodavo.ui.theme.DodavoTheme
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch

/**
 * The one activity, edge to edge; its screens are composables (Navigation 3).
 * An AppCompatActivity, for the app's own language on every version.
 */
class MainActivity : AppCompatActivity() {
    private val requests = MutableStateFlow<Request?>(null)

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        if (savedInstanceState == null) handle(intent)
        setContent {
            DodavoTheme {
                DodavoNav(requests)
            }
        }
        // The core says what it says in the language of these texts, also
        // after the user chose another one for the app.
        val repository = (application as DodavoApp).container.repository
        lifecycleScope.launch { repository.useLanguage(getString(R.string.dodavo_lang)) }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handle(intent)
    }

    /** Where a notification or another app sends the user. */
    private fun handle(intent: Intent?) {
        intent ?: return
        requests.value = when {
            intent.hasExtra(EXTRA_OFFER) -> Request.ShowOffer(intent.getLongExtra(EXTRA_OFFER, -1))
            intent.getBooleanExtra(EXTRA_TRANSFERS, false) -> Request.ShowTransfers
            intent.action == Intent.ACTION_SEND ->
                IntentCompat.getParcelableExtra(intent, Intent.EXTRA_STREAM, Uri::class.java)
                    ?.let { Request.Share(listOf(it.toString())) }
            intent.action == Intent.ACTION_SEND_MULTIPLE ->
                IntentCompat.getParcelableArrayListExtra(intent, Intent.EXTRA_STREAM, Uri::class.java)
                    ?.takeIf { it.isNotEmpty() }
                    ?.let { uris -> Request.Share(uris.map(Uri::toString)) }
            else -> null
        } ?: requests.value
    }

    companion object {
        const val EXTRA_OFFER = "com.hrvojekatic.dodavo.OFFER"
        const val EXTRA_TRANSFERS = "com.hrvojekatic.dodavo.TRANSFERS"

        /** Whether Android has a per-app language page of its own (13 and later). */
        val systemLanguagePage = Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU
    }
}
