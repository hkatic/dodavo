package com.hrvojekatic.dodavo

import androidx.annotation.StringRes
import androidx.compose.ui.test.ExperimentalTestApi
import androidx.compose.ui.test.hasClickAction
import androidx.compose.ui.test.hasContentDescription
import androidx.compose.ui.test.hasSetTextAction
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.accessibility.enableAccessibilityChecks
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.test.tryPerformAccessibilityChecks
import android.Manifest
import android.os.Build
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.rule.GrantPermissionRule
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/**
 * Goes through the app's screens with the Accessibility Test Framework's
 * checks on, which every action runs on the whole screen: names of controls,
 * touch targets, contrast and the rest. Runs on a device or emulator, with
 * the app's own core. The test run installs the app anew and uninstalls it
 * afterwards, with its Contact ID: run it on an emulator (ANDROID_SERIAL).
 */
@OptIn(ExperimentalTestApi::class)
@RunWith(AndroidJUnit4::class)
class ScreensAccessibilityTest {
    /** What the first start would ask for, so that Android's question does not cover the screens. */
    @get:Rule(order = 0)
    val permissions: GrantPermissionRule = GrantPermissionRule.grant(
        *buildList {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) add(Manifest.permission.POST_NOTIFICATIONS)
            if (Build.VERSION.SDK_INT >= 37) add("android.permission.ACCESS_LOCAL_NETWORK")
            if (Build.VERSION.SDK_INT <= Build.VERSION_CODES.Q) add(Manifest.permission.WRITE_EXTERNAL_STORAGE)
        }.toTypedArray(),
    )

    @get:Rule(order = 1)
    val rule = createAndroidComposeRule<MainActivity>()

    private fun text(@StringRes id: Int) = rule.activity.getString(id)

    private fun tap(@StringRes id: Int) {
        val node = rule.onNode(hasText(text(id)) and hasClickAction())
        // Into view, where it is in a list that scrolls.
        runCatching { node.performScrollTo() }
        node.performClick()
        rule.waitForIdle()
    }

    private fun tapDescribed(@StringRes id: Int) {
        rule.onNode(hasContentDescription(text(id)) and hasClickAction()).performClick()
        rule.waitForIdle()
    }

    @Before
    fun start() {
        rule.enableAccessibilityChecks()
        // The core is running once its screens can add a recipient.
        rule.waitUntil(15_000) {
            rule.onAllNodes(hasText(text(R.string.add_recipient))).fetchSemanticsNodes().isNotEmpty()
        }
    }

    @Test
    fun recipientsAndTheirEditor() {
        rule.onRoot().tryPerformAccessibilityChecks()
        tap(R.string.add_recipient)
        tapDescribed(R.string.close)
    }

    /** A recipient in the list, its menu and the question before it goes. */
    @Test
    fun aRecipientAndItsMenu() {
        val name = "Checks"
        tap(R.string.add_recipient)
        rule.onNode(hasSetTextAction() and hasText(text(R.string.recipient_name_label))).performTextInput(name)
        rule.onNode(hasSetTextAction() and hasText(text(R.string.contact_id_label))).performTextInput(SOME_ID)
        tap(R.string.save)
        rule.waitUntil(5_000) { rule.onAllNodes(hasText(name)).fetchSemanticsNodes().isNotEmpty() }
        rule.onRoot().tryPerformAccessibilityChecks()
        rule.onNode(hasContentDescription(name, substring = true) and hasClickAction()).performClick()
        tap(R.string.action_remove)
        rule.onNode(hasText(text(R.string.remove)) and hasClickAction()).performClick()
        rule.waitForIdle()
    }

    @Test
    fun settingsAndBlockedSenders() {
        tapDescribed(R.string.more_options)
        tap(R.string.menu_settings)
        tap(R.string.blocked_senders)
        tapDescribed(R.string.navigate_up)
        tapDescribed(R.string.navigate_up)
    }

    @Test
    fun aboutAndLicenses() {
        tapDescribed(R.string.more_options)
        tap(R.string.menu_about)
        tap(R.string.licenses)
        tapDescribed(R.string.navigate_up)
        tapDescribed(R.string.navigate_up)
    }

    private companion object {
        /** A Contact ID that is not this device's. */
        const val SOME_ID = "5c4e39157986d61cf5c7e137536709a8d8676d139e03b954110c7840167d8a69"
    }

    @Test
    fun transfers() {
        rule.onNode(hasText(text(R.string.nav_transfers)) and hasClickAction()).performClick()
        rule.waitForIdle()
        rule.onRoot().tryPerformAccessibilityChecks()
        rule.onNode(hasText(text(R.string.nav_recipients)) and hasClickAction()).performClick()
    }
}
