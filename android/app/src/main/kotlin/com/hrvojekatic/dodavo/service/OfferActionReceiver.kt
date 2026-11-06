package com.hrvojekatic.dodavo.service

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import com.hrvojekatic.dodavo.ui.common.container
import kotlinx.coroutines.launch

/** Decline, from an offer's notification. */
class OfferActionReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val id = intent.getLongExtra(EXTRA_OFFER, -1)
        if (intent.action != ACTION_DECLINE || id < 0) return
        val pending = goAsync()
        val container = context.container
        container.scope.launch {
            try {
                container.repository.decline(id)
                Notifications.cancelOffer(context, id)
            } finally {
                pending.finish()
            }
        }
    }

    companion object {
        const val ACTION_DECLINE = "com.hrvojekatic.dodavo.DECLINE"
        const val EXTRA_OFFER = "offer"
    }
}
