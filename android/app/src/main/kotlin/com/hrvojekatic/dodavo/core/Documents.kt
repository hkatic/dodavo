package com.hrvojekatic.dodavo.core

import android.content.ContentResolver
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.system.OsConstants
import androidx.core.net.toUri
import java.io.FileNotFoundException

/**
 * The documents that the user chose in Android's pickers, which the core
 * reads where they are (`dodavo_core::source`) but cannot open itself: it
 * calls [open]. The permission to read a file is kept across restarts while
 * a send reads it, and let go of when the core says that none does.
 */
object Documents {
    private lateinit var resolver: ContentResolver

    /** Before the core starts. */
    fun init(context: Context) {
        resolver = context.applicationContext.contentResolver
    }

    /**
     * Opens [uri] for reading, for the core, on a thread of its own: a file
     * descriptor that the caller owns, or a negative error number.
     */
    @JvmStatic
    fun open(uri: String): Int = try {
        resolver.openFileDescriptor(uri.toUri(), "r")?.detachFd() ?: -OsConstants.ENOENT
    } catch (_: FileNotFoundException) {
        -OsConstants.ENOENT
    } catch (_: SecurityException) {
        -OsConstants.EACCES
    } catch (_: Exception) {
        -OsConstants.EIO
    }

    /** Keeps the permission to read [uri] across restarts; whether Android allowed that. */
    fun keep(uri: Uri): Boolean = try {
        resolver.takePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION)
        true
    } catch (_: SecurityException) {
        false
    }

    /** Lets go of the permission to read [uri], if it was kept. */
    fun release(uri: String) {
        try {
            resolver.releasePersistableUriPermission(uri.toUri(), Intent.FLAG_GRANT_READ_URI_PERMISSION)
        } catch (_: SecurityException) {
            // Not kept, or gone already.
        }
    }

    /** The documents whose permission is kept. */
    fun kept(): List<String> = resolver.persistedUriPermissions.map { it.uri.toString() }
}
