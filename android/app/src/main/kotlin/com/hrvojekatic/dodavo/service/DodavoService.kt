package com.hrvojekatic.dodavo.service

import android.app.Notification
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
import com.hrvojekatic.dodavo.R
import com.hrvojekatic.dodavo.core.Row
import com.hrvojekatic.dodavo.ui.common.container
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.FlowPreview
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.sample
import kotlinx.coroutines.launch

/**
 * Keeps Dodavo running while data moves (`dataSync`), and all the time with
 * "Receive in the background" on (`connectedDevice`: transfers with the
 * user's own computers and contacts). Its notification says which.
 */
class DodavoService : Service() {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)

    /** What the service is for now. */
    private data class Need(val moving: List<Row>, val background: Boolean) {
        val busy get() = moving.isNotEmpty()
        val any get() = busy || background
    }

    private var need = Need(emptyList(), false)
    /** Android 15 and later: the time for `dataSync` is used up for today. */
    private var dataSyncTimedOut = false

    override fun onBind(intent: Intent?): IBinder? = null

    @OptIn(FlowPreview::class)
    override fun onCreate() {
        super.onCreate()
        Notifications.createChannels(this)
        val container = container
        scope.launch {
            combine(container.repository.transfers, container.preferences.receiveInBackground) { rows, background ->
                Need(rows.values.filter { it.movingData }.sortedBy { it.id }, background)
            }.sample(1_000).collect { update(it) }
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        // Promoted at once, as Android asks, with what is known now.
        promote(need.copy(background = need.background || intent?.getBooleanExtra(EXTRA_BACKGROUND, false) == true))
        return START_STICKY
    }

    private fun update(next: Need) {
        need = next
        if (!next.any) {
            ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_REMOVE)
            stopSelf()
            return
        }
        promote(next)
    }

    private fun promote(need: Need) {
        var type = 0
        if (need.busy && !dataSyncTimedOut) type = type or ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC
        if (need.background || type == 0) type = type or ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE
        runCatching { ServiceCompat.startForeground(this, Notifications.SERVICE_ID, notification(need), type) }
    }

    override fun onTimeout(startId: Int, fgsType: Int) {
        // The day's time for moving data in the background is used up. The
        // transfer waits, as after a lost connection, until Dodavo is in
        // front again, or goes on under "Receive in the background".
        dataSyncTimedOut = true
        if (need.background) promote(need) else {
            ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_REMOVE)
            stopSelf()
        }
    }

    private fun notification(need: Need): Notification {
        val open = Notifications.openApp(this)
        val builder = if (need.busy) {
            val row = need.moving.singleOrNull()
            NotificationCompat.Builder(this, Notifications.CHANNEL_TRANSFERS)
                .setContentTitle(row?.item ?: getString(R.string.notify_transfers))
                .setContentText(row?.let { listOf(it.status, it.amount).filter(String::isNotEmpty).joinToString(" – ") })
                .apply {
                    val fraction = row?.fraction
                    if (fraction != null) setProgress(100, (fraction * 100).toInt(), false) else setProgress(0, 0, true)
                }
        } else {
            NotificationCompat.Builder(this, Notifications.CHANNEL_BACKGROUND)
                .setContentTitle(getString(R.string.notify_ready))
                .setContentText(getString(R.string.notify_ready_text))
        }
        return builder
            .setSmallIcon(R.drawable.ic_notification)
            .setContentIntent(open)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setForegroundServiceBehavior(NotificationCompat.FOREGROUND_SERVICE_IMMEDIATE)
            .build()
    }

    override fun onDestroy() {
        scope.cancel()
        super.onDestroy()
    }

    companion object {
        private const val EXTRA_BACKGROUND = "background"

        /**
         * Starts the service, if Android lets it start now: it does while
         * Dodavo is in front, after a restart of the phone and from a tap on
         * a notification. Whether it started.
         */
        fun start(context: Context, background: Boolean = false): Boolean = runCatching {
            ContextCompat.startForegroundService(
                context,
                Intent(context, DodavoService::class.java).putExtra(EXTRA_BACKGROUND, background),
            )
        }.isSuccess

        /** Whether Android has the types this service needs (from 14 on). */
        val typed = Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE
    }
}
