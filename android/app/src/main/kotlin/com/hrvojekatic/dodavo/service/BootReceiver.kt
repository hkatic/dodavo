package com.hrvojekatic.dodavo.service

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import com.hrvojekatic.dodavo.ui.common.container
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch

/**
 * After a restart of the phone, or an update of the app: with "Receive in
 * the background" on, Dodavo is ready again (the core started with the
 * process, and transfers that were under way go on).
 */
class BootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != Intent.ACTION_BOOT_COMPLETED && intent.action != Intent.ACTION_MY_PACKAGE_REPLACED) return
        val pending = goAsync()
        val container = context.container
        container.scope.launch {
            try {
                if (container.preferences.receiveInBackground.first()) DodavoService.start(context)
            } finally {
                pending.finish()
            }
        }
    }
}
