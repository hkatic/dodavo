package com.hrvojekatic.dodavo.core

import android.content.Context
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/** What the core needs to start (`core::Options` in Rust). */
@Serializable
data class StartOptions(
    @SerialName("data_dir") val dataDir: String,
    @SerialName("temp_dir") val tempDir: String,
    @SerialName("downloads_dir") val downloadsDir: String,
    /** The language of the app's texts (`R.string.dodavo_lang`). */
    val language: String,
    val languages: List<String>,
    @SerialName("device_name") val deviceName: String?,
)

/**
 * Dodavo's core, behind JSON: [NativeCore] in the app, a fake in tests.
 * Everything here may block, so it is called off the main thread.
 */
interface DodavoCore {
    /** Starts the core once; a later start gets the first answer. */
    fun start(options: StartOptions): Started

    /** Does what [call] says; the answer, as JSON. */
    fun call(call: Call): String

    /** The next message, waiting for one at most [timeoutMs]. */
    fun next(timeoutMs: Long): Message?
}

/** The JSON of the core. */
internal val coreJson = Json {
    // Both sides are built together; a field that one of them does not know
    // yet must not make the app stop.
    ignoreUnknownKeys = true
    explicitNulls = false
}

private val callJson = Json(coreJson) {
    classDiscriminator = "call"
    encodeDefaults = true
}

/** The calls as the core reads them. */
internal fun Call.toJson(): String = callJson.encodeToString(Call.serializer(), this)

/** The core in `libdodavo.so`. */
class NativeCore(private val context: Context) : DodavoCore {
    override fun start(options: StartOptions): Started {
        val reply = Native.start(
            context.applicationContext,
            coreJson.encodeToString(StartOptions.serializer(), options),
        )
        return coreJson.decodeFromString(Started.serializer(), reply)
    }

    override fun call(call: Call): String = Native.call(call.toJson())

    override fun next(timeoutMs: Long): Message? =
        Native.next(timeoutMs)?.let { coreJson.decodeFromString(Message.serializer(), it) }
}
