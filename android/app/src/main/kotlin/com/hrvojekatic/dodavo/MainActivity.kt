package com.hrvojekatic.dodavo

import android.os.Bundle
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.appcompat.app.AppCompatActivity
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.hrvojekatic.dodavo.core.CoreState

/** The one activity; its screens are composables (Navigation 3). */
class MainActivity : AppCompatActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        val repository = (application as DodavoApp).container.repository
        setContent {
            MaterialTheme {
                val state by repository.state.collectAsStateWithLifecycle()
                val status by repository.status.collectAsStateWithLifecycle()
                Scaffold { padding ->
                    Column(Modifier.fillMaxSize().padding(padding).padding(16.dp)) {
                        when (val s = state) {
                            CoreState.Starting -> Text("…")
                            is CoreState.Failed -> Text(s.reason)
                            is CoreState.Running -> {
                                Text(s.started.myId)
                                Text(status?.text ?: "")
                            }
                        }
                    }
                }
            }
        }
    }
}
