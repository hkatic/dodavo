package com.hrvojekatic.dodavo.core

import android.content.Context

/**
 * Dodavo's Rust core (`crates/dodavo-android`, `libdodavo.so`): JSON in and
 * out, as [Calls] and [Messages] describe it.
 */
internal object Native {
    init {
        System.loadLibrary("dodavo")
    }

    /** Starts the core once; later calls get the first answer ([Started]). */
    @JvmStatic
    external fun start(context: Context, options: String): String

    /** Does what [call] says and returns the answer. May touch the disk. */
    @JvmStatic
    external fun call(call: String): String

    /** The next message, or null. */
    @JvmStatic
    external fun poll(): String?

    /** The next message, waiting for one at most [timeoutMs], or null. */
    @JvmStatic
    external fun next(timeoutMs: Long): String?
}
