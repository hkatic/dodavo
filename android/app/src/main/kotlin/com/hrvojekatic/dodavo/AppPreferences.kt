package com.hrvojekatic.dodavo

import android.content.Context
import androidx.datastore.preferences.core.booleanPreferencesKey
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.preferencesDataStore
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map

private val Context.dataStore by preferencesDataStore("preferences")

/**
 * What only the Android app keeps: Dodavo's own settings are in the core
 * (config.json), shared with the other systems.
 */
class AppPreferences(private val context: Context) {
    private val receiveKey = booleanPreferencesKey("receive_in_background")
    private val askedKey = booleanPreferencesKey("asked_for_permissions")

    /** "Receive in the background": the service runs all the time. */
    val receiveInBackground: Flow<Boolean> = context.dataStore.data.map { it[receiveKey] ?: false }

    suspend fun setReceiveInBackground(on: Boolean) {
        context.dataStore.edit { it[receiveKey] = on }
    }

    /** Whether the permissions were asked for at the first start. */
    suspend fun askedForPermissions(): Boolean = context.dataStore.data.first()[askedKey] ?: false

    suspend fun setAskedForPermissions() {
        context.dataStore.edit { it[askedKey] = true }
    }
}
