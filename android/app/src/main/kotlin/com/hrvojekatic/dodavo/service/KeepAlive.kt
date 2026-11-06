package com.hrvojekatic.dodavo.service

import android.content.Context
import android.net.ConnectivityManager
import android.net.LinkProperties
import android.net.Network
import android.net.wifi.WifiManager
import androidx.lifecycle.DefaultLifecycleObserver
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.ProcessLifecycleOwner
import com.hrvojekatic.dodavo.AppPreferences
import com.hrvojekatic.dodavo.core.CoreRepository
import com.hrvojekatic.dodavo.core.Documents
import com.hrvojekatic.dodavo.core.Event
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.FlowPreview
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.debounce
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch

/**
 * What keeps Dodavo working outside its screens: the service while it is
 * needed, the notifications while Dodavo is not in front, the network's
 * changes for the core, and the Wi-Fi multicast lock that lets mDNS find the
 * computers on the local network.
 */
class KeepAlive(
    private val context: Context,
    private val repository: CoreRepository,
    private val preferences: AppPreferences,
    private val scope: CoroutineScope,
) {
    private val inFront = MutableStateFlow(false)
    private val networkChanges = MutableStateFlow(0L)
    private val multicast = context.applicationContext.getSystemService(WifiManager::class.java)
        ?.createMulticastLock("dodavo-mdns")?.apply { setReferenceCounted(false) }

    @OptIn(FlowPreview::class)
    fun start() {
        Notifications.createChannels(context)
        ProcessLifecycleOwner.get().lifecycle.addObserver(object : DefaultLifecycleObserver {
            override fun onStart(owner: LifecycleOwner) {
                inFront.value = true
                // Back from the background, perhaps from sleep: the network
                // may have changed meanwhile.
                networkChanges.value += 1
            }

            override fun onStop(owner: LifecycleOwner) {
                inFront.value = false
            }
        })

        // The service: while data moves or with "Receive in the background".
        scope.launch(Dispatchers.Main) {
            combine(
                repository.transfers.map { rows -> rows.values.any { it.movingData } },
                preferences.receiveInBackground,
                inFront,
            ) { moving, background, front -> Triple(moving, background, front) }
                .distinctUntilChanged()
                .collect { (moving, background, front) ->
                    if (moving || background) DodavoService.start(context, background)
                    if (front || background || moving) multicast?.acquire() else multicast?.release()
                }
        }

        // What happens while Dodavo is not in front comes as notifications.
        scope.launch(Dispatchers.Main) {
            repository.events.collect { event ->
                when (event) {
                    // An offer always: it waits for an answer.
                    is Event.OfferArrived -> if (!inFront.value) Notifications.offer(context, event.offer, event.notice)
                    is Event.OfferGone -> {
                        Notifications.cancelOffer(context, event.id)
                        if (!inFront.value) Notifications.notice(context, event.id, event.notice)
                    }
                    is Event.TransferNotice -> if (!inFront.value) Notifications.notice(context, event.id, event.notice)
                    is Event.Error -> {}
                    // The permission to read it goes, wherever Dodavo is.
                    is Event.DocumentDone -> Documents.release(event.uri)
                }
            }
        }

        // An offer that is answered, wherever that was, or that its sender
        // withdrew, takes its notification along.
        scope.launch(Dispatchers.Main) {
            var waiting = emptySet<Long>()
            repository.offers.collect { offers ->
                (waiting - offers.keys).forEach { Notifications.cancelOffer(context, it) }
                waiting = offers.keys
            }
        }

        // The network: Android tells only the app, iroh has to hear it.
        context.getSystemService(ConnectivityManager::class.java)?.registerDefaultNetworkCallback(
            object : ConnectivityManager.NetworkCallback() {
                override fun onAvailable(network: Network) { networkChanges.value += 1 }
                override fun onLost(network: Network) { networkChanges.value += 1 }
                override fun onLinkPropertiesChanged(network: Network, properties: LinkProperties) {
                    networkChanges.value += 1
                }
            },
        )
        scope.launch {
            networkChanges.debounce(500).collect { if (it > 0) repository.networkChanged() }
        }
    }
}
