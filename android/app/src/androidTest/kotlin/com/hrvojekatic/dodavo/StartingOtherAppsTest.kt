package com.hrvojekatic.dodavo

import android.Manifest
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.os.Build
import androidx.activity.result.contract.ActivityResultContract
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.rule.GrantPermissionRule
import com.hrvojekatic.dodavo.ui.common.launchOrTell
import com.hrvojekatic.dodavo.ui.common.startOrTell
import com.hrvojekatic.dodavo.ui.common.tryStart
import org.junit.Assert.assertFalse
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/**
 * What the app does where another app is missing or may not be started: it
 * says so rather than end. Nothing here starts another app. Runs with the
 * app's own core, which makes a Contact ID: on an emulator, or a device
 * without Dodavo's own data.
 */
@RunWith(AndroidJUnit4::class)
class StartingOtherAppsTest {
    /** What the first start would ask for, so that Android's question does not cover the app. */
    @get:Rule
    val permissions: GrantPermissionRule = GrantPermissionRule.grant(
        *buildList {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) add(Manifest.permission.POST_NOTIFICATIONS)
            if (Build.VERSION.SDK_INT >= 37) add("android.permission.ACCESS_LOCAL_NETWORK")
            if (Build.VERSION.SDK_INT <= Build.VERSION_CODES.Q) add(Manifest.permission.WRITE_EXTERNAL_STORAGE)
        }.toTypedArray(),
    )

    /** What nothing on any device takes. */
    private val nothingTakes = Intent("com.hrvojekatic.dodavo.test.NOTHING_TAKES_THIS")

    /** An activity of the test package, not exported (androidTest/AndroidManifest.xml). */
    private val notExported: Intent
        get() = Intent().setComponent(
            ComponentName(InstrumentationRegistry.getInstrumentation().context.packageName, "android.app.Activity"),
        )

    /** A picker whose intent nothing takes, as on a device without one. */
    private object NoPicker : ActivityResultContract<Unit, Unit>() {
        override fun createIntent(context: Context, input: Unit) = Intent("com.hrvojekatic.dodavo.test.NO_PICKER")
        override fun parseResult(resultCode: Int, intent: Intent?) = Unit
    }

    private fun onActivity(block: (MainActivity) -> Unit) {
        ActivityScenario.launch(MainActivity::class.java).use { it.onActivity(block) }
    }

    @Test
    fun anIntentThatNothingTakesIsNotStarted() = onActivity { activity ->
        assertFalse(activity.tryStart(nothingTakes))
    }

    @Test
    fun anActivityThatMayNotBeStartedIsNotStarted() = onActivity { activity ->
        assertFalse(activity.tryStart(notExported))
    }

    @Test
    fun whatCannotStartIsTold() = onActivity { activity ->
        activity.startOrTell(nothingTakes)
        activity.startOrTell(notExported, R.string.screen_unavailable)
    }

    @Test
    fun aMissingPickerIsTold() = onActivity { activity ->
        val launcher = activity.activityResultRegistry.register("no-picker", NoPicker) {}
        try {
            activity.launchOrTell(launcher, Unit, Intent.ACTION_OPEN_DOCUMENT, R.string.no_file_picker)
        } finally {
            launcher.unregister()
        }
    }
}
