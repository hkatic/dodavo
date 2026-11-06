package com.hrvojekatic.dodavo

import android.os.Bundle
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.appcompat.app.AppCompatActivity
import com.hrvojekatic.dodavo.ui.DodavoNav
import com.hrvojekatic.dodavo.ui.theme.DodavoTheme

/**
 * The one activity, edge to edge; its screens are composables (Navigation 3).
 * An AppCompatActivity, for the app's own language on every version.
 */
class MainActivity : AppCompatActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        setContent {
            DodavoTheme {
                DodavoNav()
            }
        }
    }
}
