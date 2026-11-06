package com.hrvojekatic.dodavo.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.List
import androidx.compose.material.icons.filled.Person
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.adaptive.navigationsuite.NavigationSuiteScaffold
import androidx.compose.material3.adaptive.navigationsuite.NavigationSuiteScaffoldDefaults
import androidx.compose.material3.adaptive.navigationsuite.NavigationSuiteType
import androidx.compose.material3.adaptive.currentWindowAdaptiveInfoV2
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.navigation3.rememberViewModelStoreNavEntryDecorator
import androidx.navigation3.runtime.NavBackStack
import androidx.navigation3.runtime.NavKey
import androidx.navigation3.runtime.entryProvider
import androidx.navigation3.runtime.rememberNavBackStack
import androidx.navigation3.runtime.rememberSaveableStateHolderNavEntryDecorator
import androidx.navigation3.ui.NavDisplay
import com.hrvojekatic.dodavo.R
import com.hrvojekatic.dodavo.core.Offer
import com.hrvojekatic.dodavo.ui.about.AboutScreen
import com.hrvojekatic.dodavo.ui.about.LicensesScreen
import com.hrvojekatic.dodavo.ui.common.container
import com.hrvojekatic.dodavo.ui.help.HelpScreen
import com.hrvojekatic.dodavo.ui.incoming.IncomingScreen
import com.hrvojekatic.dodavo.ui.recipients.RecipientEditorScreen
import com.hrvojekatic.dodavo.ui.recipients.RecipientsScreen
import com.hrvojekatic.dodavo.ui.recipients.SendToScreen
import com.hrvojekatic.dodavo.ui.settings.BlockedScreen
import com.hrvojekatic.dodavo.ui.settings.SettingsScreen
import com.hrvojekatic.dodavo.ui.transfers.TransfersScreen
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.serialization.Serializable

/** The screens. */
@Serializable data object RecipientsKey : NavKey
@Serializable data object TransfersKey : NavKey
/** Add a recipient, or edit the one with [contactId]. */
@Serializable data class RecipientEditorKey(val contactId: String? = null) : NavKey
@Serializable data class IncomingKey(val offerId: Long) : NavKey
@Serializable data object SettingsKey : NavKey
@Serializable data object BlockedKey : NavKey
@Serializable data object AboutKey : NavKey
@Serializable data object LicensesKey : NavKey
@Serializable data object HelpKey : NavKey
/** Items shared from another app, by their URIs: to whom? */
@Serializable data class SendToKey(val uris: List<String>) : NavKey

/** Where the activity was asked to go (a notification, another app). */
sealed interface Request {
    data class ShowOffer(val id: Long) : Request
    data object ShowTransfers : Request
    data class Share(val uris: List<String>) : Request
}

/** Where the screens go: forward, back, or to a destination of the navigation bar. */
class Navigator(private val backStack: NavBackStack<NavKey>) {
    fun go(key: NavKey) {
        backStack.add(key)
    }

    fun back() {
        if (backStack.size > 1) backStack.removeAt(backStack.lastIndex)
    }

    /** A destination of the navigation bar, from which back leaves the app. */
    fun top(key: NavKey) {
        backStack.clear()
        backStack.add(key)
    }

    /** The offer [id], once, above whatever shows. */
    fun incoming(id: Long) {
        if (backStack.lastOrNull() != IncomingKey(id)) go(IncomingKey(id))
    }
}

/** The app: a navigation bar (or rail) for Recipients and Transfers, and the other screens above them. */
@Composable
fun DodavoNav(
    requests: MutableStateFlow<Request?>,
    backStack: NavBackStack<NavKey> = rememberNavBackStack(RecipientsKey),
) {
    val navigator = Navigator(backStack)
    val request by requests.collectAsStateWithLifecycle()
    LaunchedEffect(request) {
        when (val r = request) {
            is Request.ShowOffer -> navigator.incoming(r.id)
            Request.ShowTransfers -> navigator.top(TransfersKey)
            is Request.Share -> navigator.go(SendToKey(r.uris))
            null -> {}
        }
        requests.value = null
    }
    FirstStartPermissions()
    val current = backStack.lastOrNull()
    val topLevel = current == RecipientsKey || current == TransfersKey
    val layout = if (topLevel) {
        NavigationSuiteScaffoldDefaults.calculateFromAdaptiveInfo(currentWindowAdaptiveInfoV2())
    } else {
        NavigationSuiteType.None
    }
    NavigationSuiteScaffold(
        layoutType = layout,
        navigationSuiteItems = {
            item(
                selected = current == RecipientsKey,
                onClick = { navigator.top(RecipientsKey) },
                icon = { Icon(Icons.Filled.Person, contentDescription = null) },
                label = { Text(stringResource(R.string.nav_recipients)) },
            )
            item(
                selected = current == TransfersKey,
                onClick = { navigator.top(TransfersKey) },
                icon = { Icon(Icons.AutoMirrored.Filled.List, contentDescription = null) },
                label = { Text(stringResource(R.string.nav_transfers)) },
            )
        },
    ) {
        NavDisplay(
            backStack = backStack,
            onBack = { navigator.back() },
            entryDecorators = listOf(
                rememberSaveableStateHolderNavEntryDecorator(),
                rememberViewModelStoreNavEntryDecorator(),
            ),
            entryProvider = entryProvider {
                entry<RecipientsKey> { RecipientsScreen(navigator) }
                entry<TransfersKey> { TransfersScreen(navigator) }
                entry<RecipientEditorKey> { RecipientEditorScreen(it.contactId, navigator) }
                entry<IncomingKey> { IncomingScreen(it.offerId, navigator) }
                entry<SettingsKey> { SettingsScreen(navigator) }
                entry<BlockedKey> { BlockedScreen(navigator) }
                entry<AboutKey> { AboutScreen(navigator) }
                entry<LicensesKey> { LicensesScreen(navigator) }
                entry<HelpKey> { HelpScreen(navigator) }
                entry<SendToKey> { SendToScreen(it.uris, navigator) }
            },
        )
    }
}

/**
 * What waits for an answer, at the top of the main screens. It does not take
 * the focus: the status line says that it came, and a notification too.
 */
@Composable
fun OfferBanner(offers: List<Offer>, onReview: (Offer) -> Unit, modifier: Modifier = Modifier) {
    val offer = offers.firstOrNull() ?: return
    Card(
        modifier = modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 8.dp),
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.secondaryContainer),
    ) {
        Column(Modifier.padding(16.dp)) {
            Text(offer.headline, style = MaterialTheme.typography.titleMedium)
            Text(offer.description, style = MaterialTheme.typography.bodyMedium)
            Button(onClick = { onReview(offer) }, modifier = Modifier.padding(top = 8.dp)) {
                Text(stringResource(R.string.incoming_title))
            }
        }
    }
}

/** The waiting offers, oldest first. */
@Composable
fun waitingOffers(): List<Offer> {
    val repository = androidx.compose.ui.platform.LocalContext.current.container.repository
    val offers by repository.offers.collectAsStateWithLifecycle()
    return offers.values.sortedBy { it.id }
}

/**
 * At the first start, what Dodavo needs from Android: notifications for
 * what arrives while it is not in front (13 and later), the local network
 * for direct connections (17 and later), and storage for received items
 * (8 to 10, where Download is not open to apps without it).
 */
@Composable
private fun FirstStartPermissions() {
    val context = androidx.compose.ui.platform.LocalContext.current
    val preferences = context.container.preferences
    val ask = androidx.activity.compose.rememberLauncherForActivityResult(
        androidx.activity.result.contract.ActivityResultContracts.RequestMultiplePermissions(),
    ) {}
    LaunchedEffect(Unit) {
        if (preferences.askedForPermissions()) return@LaunchedEffect
        val wanted = buildList {
            val sdk = android.os.Build.VERSION.SDK_INT
            if (android.os.Build.VERSION.SDK_INT >= android.os.Build.VERSION_CODES.TIRAMISU) {
                add(android.Manifest.permission.POST_NOTIFICATIONS)
            }
            if (sdk >= 37) add("android.permission.ACCESS_LOCAL_NETWORK")
            if (sdk <= 29) add(android.Manifest.permission.WRITE_EXTERNAL_STORAGE)
        }.filter {
            androidx.core.content.ContextCompat.checkSelfPermission(context, it) !=
                android.content.pm.PackageManager.PERMISSION_GRANTED
        }
        preferences.setAskedForPermissions()
        if (wanted.isNotEmpty()) ask.launch(wanted.toTypedArray())
    }
}
