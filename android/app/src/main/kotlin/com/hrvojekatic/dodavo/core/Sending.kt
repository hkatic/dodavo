package com.hrvojekatic.dodavo.core

import android.content.Context
import android.net.Uri
import android.os.ParcelFileDescriptor
import android.provider.DocumentsContract
import android.provider.OpenableColumns
import android.system.ErrnoException
import android.system.Os
import android.system.OsConstants
import com.hrvojekatic.dodavo.R
import java.io.File
import java.util.UUID
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * Hands what the user picked, or shared from another app, to the core. A
 * picked file is read where it is, with the permission to read it kept for
 * as long as its transfer waits, also after a restart (see [Documents]); a
 * picked folder is packed straight from where it is, as the app lists it.
 * What another app shared, and a file that cannot be read that way (one
 * that comes as a stream, from a cloud say), goes as a copy in the app's own
 * storage under its name and with its date, which the transfer deletes.
 */
class Sending(
    private val context: Context,
    private val repository: CoreRepository,
    private val io: CoroutineDispatcher = Dispatchers.IO,
) {
    private val staging = File(context.noBackupFilesDir, "tmp/staging")

    /**
     * Sends each of the documents [uris] to each of [peerIds]; an error text
     * if that could not be done. [shared]: another app shared them, and they
     * can be read only while this screen lasts, so they go as copies. A copy
     * belongs to the transfer that sends it, which deletes it, so every
     * recipient gets one of their own.
     */
    suspend fun sendFiles(peerIds: List<String>, uris: List<Uri>, shared: Boolean = false): String? {
        if (!shared) makeRoom(uris.size)
        val (inPlace, toCopy) = withContext(io) {
            uris.partition { !shared && readsInPlace(it) }
        }
        val documents = withContext(io) {
            inPlace.map { uri ->
                val (name, modified) = describe(uri)
                Document(uri = uri.toString(), name = name, modified = modified)
            }
        }
        // For each item, its copies in the order of the recipients.
        val copied = withContext(io) {
            copies { made ->
                toCopy.map { uri ->
                    val (name, modified) = describe(uri)
                    var first: File? = null
                    peerIds.map {
                        val file = File(newFolder().also(made::add), name)
                        // The item is read once; the other copies are made of the first.
                        val from = first
                        if (from == null) {
                            copy(uri, file, modified)
                            first = file
                        } else {
                            from.copyTo(file)
                            modified?.takeIf { it > 0 }?.let(file::setLastModified)
                        }
                        file.path
                    }
                }
            }
        }
        if (copied == null) {
            documents.forEach { Documents.release(it.uri) }
            return context.getString(R.string.item_unreadable)
        }
        if (documents.isNotEmpty()) repository.sendDocuments(peerIds, documents)?.let { return it }
        if (copied.isEmpty()) return null
        peerIds.forEachIndexed { recipient, peerId ->
            repository.send(listOf(peerId), copied.map { it[recipient] })?.let { return it }
        }
        return null
    }

    /** Sends the folder [tree] (from the system's folder picker) to each of [peerIds], packed where it is. */
    suspend fun sendFolder(peerIds: List<String>, tree: Uri): String? {
        val document = withContext(io) {
            runCatching {
                val rootId = DocumentsContract.getTreeDocumentId(tree)
                val (name, modified) = describe(DocumentsContract.buildDocumentUriUsingTree(tree, rootId))
                val items = mutableListOf<DocumentItem>()
                list(tree, rootId, emptyList(), items)
                Document(uri = tree.toString(), name = name, modified = modified, items = items)
            }.getOrNull()
        } ?: return context.getString(R.string.item_unreadable)
        return repository.sendDocuments(peerIds, listOf(document))
    }

    /**
     * Whether [uri] can be read where it is for as long as its transfer
     * waits: Android keeps the permission, and it opens as a file, which the
     * core can read from any point (not a stream).
     */
    private fun readsInPlace(uri: Uri): Boolean {
        if (!Documents.keep(uri)) return false
        val file = runCatching {
            context.contentResolver.openFileDescriptor(uri, "r")?.use(::isFile)
        }.getOrNull() == true
        if (!file) Documents.release(uri.toString())
        return file
    }

    private fun isFile(descriptor: ParcelFileDescriptor): Boolean = try {
        descriptor.statSize >= 0 && Os.lseek(descriptor.fileDescriptor, 0, OsConstants.SEEK_SET) == 0L
    } catch (_: ErrnoException) {
        false
    }

    /**
     * Android keeps the permissions of an app to a few hundred documents: if
     * [count] more would come close, those that no send reads any more go (a
     * permission whose release a stop of the app cut short).
     */
    private suspend fun makeRoom(count: Int) {
        val kept = withContext(io) { Documents.kept() }
        if (kept.size + count <= ROOM) return
        val inUse = repository.documentsInUse().toSet()
        withContext(io) { kept.filterNot(inUse::contains).forEach(Documents::release) }
    }

    /** Adds what the folder [documentId] of [tree] holds to [items], each folder before what is in it. */
    private fun list(tree: Uri, documentId: String, path: List<String>, items: MutableList<DocumentItem>) {
        val children = DocumentsContract.buildChildDocumentsUriUsingTree(tree, documentId)
        val columns = arrayOf(
            DocumentsContract.Document.COLUMN_DOCUMENT_ID,
            DocumentsContract.Document.COLUMN_DISPLAY_NAME,
            DocumentsContract.Document.COLUMN_MIME_TYPE,
            DocumentsContract.Document.COLUMN_LAST_MODIFIED,
            DocumentsContract.Document.COLUMN_SIZE,
        )
        val cursor = context.contentResolver.query(children, columns, null, null, null)
            ?: error("$children cannot be listed")
        cursor.use {
            while (it.moveToNext()) {
                val id = it.getString(0)
                val itemPath = path + plainName(it.getString(1) ?: id)
                val modified = if (it.isNull(3)) null else it.getLong(3)
                if (it.getString(2) == DocumentsContract.Document.MIME_TYPE_DIR) {
                    items += DocumentItem(path = itemPath, folder = true, modified = modified)
                    list(tree, id, itemPath, items)
                } else {
                    items += DocumentItem(
                        path = itemPath,
                        size = if (it.isNull(4)) 0 else it.getLong(4),
                        modified = modified,
                        uri = DocumentsContract.buildDocumentUriUsingTree(tree, id).toString(),
                    )
                }
            }
        }
    }

    /** Runs [copy], which notes the folders it makes; if it fails, they go and the result is null. */
    private fun <T> copies(copy: (MutableList<File>) -> T): T? {
        val made = mutableListOf<File>()
        return try {
            copy(made)
        } catch (e: Exception) {
            made.forEach { it.deleteRecursively() }
            null
        }
    }

    private fun newFolder(): File = File(staging, UUID.randomUUID().toString()).apply { mkdirs() }

    /** The name a document shows, made a single name, and when it was changed. */
    private fun describe(uri: Uri): Pair<String, Long?> {
        var name: String? = null
        var modified: Long? = null
        context.contentResolver.query(uri, null, null, null, null)?.use { cursor ->
            if (cursor.moveToFirst()) {
                cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME).takeIf { it >= 0 }
                    ?.let { name = cursor.getString(it) }
                cursor.getColumnIndex(DocumentsContract.Document.COLUMN_LAST_MODIFIED).takeIf { it >= 0 }
                    ?.let { if (!cursor.isNull(it)) modified = cursor.getLong(it) }
            }
        }
        return plainName(name ?: uri.lastPathSegment ?: "item") to modified
    }

    private fun copy(uri: Uri, to: File, modified: Long?) {
        val input = context.contentResolver.openInputStream(uri) ?: error("$uri cannot be read")
        input.use { from -> to.outputStream().use { from.copyTo(it) } }
        modified?.takeIf { it > 0 }?.let(to::setLastModified)
    }

    /** A name from another app as one name here: no folders in it. */
    private fun plainName(name: String): String =
        name.replace('/', '_').replace('\u0000', '_').trim().ifEmpty { "item" }

    private companion object {
        /** Kept permissions above which those no send reads go (Android allows at least 128). */
        const val ROOM = 100
    }
}
