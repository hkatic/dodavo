package com.hrvojekatic.dodavo.service

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import com.hrvojekatic.dodavo.MainActivity
import com.hrvojekatic.dodavo.R
import com.hrvojekatic.dodavo.core.Notice
import com.hrvojekatic.dodavo.core.Offer

/** Dodavo's notifications and their channels. */
object Notifications {
    const val CHANNEL_INCOMING = "incoming"
    const val CHANNEL_TRANSFERS = "transfers"
    const val CHANNEL_FINISHED = "finished"
    const val CHANNEL_BACKGROUND = "background"

    /** The service's own notification. */
    const val SERVICE_ID = 1
    private const val OFFER_BASE = 1_000_000
    private const val NOTICE_BASE = 2_000_000
    private const val DECLINED_BASE = 3_000_000

    fun createChannels(context: Context) {
        val manager = context.getSystemService(NotificationManager::class.java)
        manager.createNotificationChannels(
            listOf(
                NotificationChannel(CHANNEL_INCOMING, context.getString(R.string.channel_incoming), NotificationManager.IMPORTANCE_HIGH),
                NotificationChannel(CHANNEL_TRANSFERS, context.getString(R.string.channel_transfers), NotificationManager.IMPORTANCE_LOW),
                NotificationChannel(CHANNEL_FINISHED, context.getString(R.string.channel_finished), NotificationManager.IMPORTANCE_DEFAULT),
                NotificationChannel(CHANNEL_BACKGROUND, context.getString(R.string.channel_background), NotificationManager.IMPORTANCE_MIN),
            ),
        )
    }

    /** Shows [notification] as [id], if Android lets Dodavo show notifications. */
    private fun post(context: Context, id: Int, notification: Notification) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) !=
            PackageManager.PERMISSION_GRANTED
        ) {
            return
        }
        NotificationManagerCompat.from(context).notify(id, notification)
    }

    /** Opens the app, at the offer [offerId] or at the transfers. */
    fun openApp(context: Context, offerId: Long? = null, request: Int = 0): PendingIntent {
        val intent = Intent(context, MainActivity::class.java)
            .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)
        if (offerId != null) intent.putExtra(MainActivity.EXTRA_OFFER, offerId) else intent.putExtra(MainActivity.EXTRA_TRANSFERS, true)
        return PendingIntent.getActivity(context, request, intent, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
    }

    /** Someone wants to send something: a tap or Review opens it; Decline declines it. */
    fun offer(context: Context, offer: Offer, notice: Notice) {
        val id = OFFER_BASE + (offer.id % OFFER_BASE).toInt()
        val decline = PendingIntent.getBroadcast(
            context,
            id,
            Intent(context, OfferActionReceiver::class.java)
                .setAction(OfferActionReceiver.ACTION_DECLINE)
                .putExtra(OfferActionReceiver.EXTRA_OFFER, offer.id),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val open = openApp(context, offer.id, id)
        val notification = NotificationCompat.Builder(context, CHANNEL_INCOMING)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(offer.headline)
            .setContentText(offer.description)
            .setSubText(notice.title)
            .setCategory(NotificationCompat.CATEGORY_MESSAGE)
            .setPriority(NotificationCompat.PRIORITY_HIGH)
            .setContentIntent(open)
            .addAction(0, context.getString(R.string.review), open)
            .addAction(0, context.getString(R.string.decline), decline)
            .setAutoCancel(true)
            .build()
        post(context, id, notification)
    }

    fun cancelOffer(context: Context, offerId: Long) {
        NotificationManagerCompat.from(context).cancel(OFFER_BASE + (offerId % OFFER_BASE).toInt())
    }

    /** What happened to a transfer while Dodavo was not in front. */
    fun notice(context: Context, id: Long, notice: Notice) {
        show(context, NOTICE_BASE + (id % OFFER_BASE).toInt(), notice)
    }

    /** An offer was declined for its size while Dodavo was not in front; [serial] tells one from the next. */
    fun declined(context: Context, serial: Long, notice: Notice) {
        show(context, DECLINED_BASE + (serial % OFFER_BASE).toInt(), notice)
    }

    private fun show(context: Context, id: Int, notice: Notice) {
        val notification = NotificationCompat.Builder(context, CHANNEL_FINISHED)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(notice.title)
            .setContentText(notice.text)
            .setStyle(NotificationCompat.BigTextStyle().bigText(notice.text))
            .setContentIntent(openApp(context, request = NOTICE_BASE))
            .setAutoCancel(true)
            .build()
        post(context, id, notification)
    }
}
