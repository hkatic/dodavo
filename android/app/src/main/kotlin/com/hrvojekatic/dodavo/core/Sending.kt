package com.hrvojekatic.dodavo.core

import android.content.Context
import android.net.Uri
import android.provider.DocumentsContract
import android.provider.OpenableColumns
import java.io.File
import java.util.UUID
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * Hands what the user picked, or shared from another app, to the core: as a
 * copy in the app's own storage under its name and with its date, which
 * stays readable for as long as the transfer waits, also after a restart.
 */
class Sending(
    private val context: Context,
    private val repository: CoreRepository,
    private val io: CoroutineDispatcher = Dispatchers.IO,
) {
    private val staging = File(context.noBackupFilesDir, "tmp/staging")

    /** Sends each of the documents [uris] to [peerId]; an error text if that could not be done. */
    suspend fun sendFiles(peerId: String, uris: List<Uri>): String? {
        val paths = withContext(io) {
            uris.map { uri ->
                val folder = newFolder()
                val (name, modified) = describe(uri)
                val file = File(folder, name)
                copy(uri, file, modified)
                file.path
            }
        }
        return repository.send(peerId, paths)
    }

    /** Sends the folder [tree] (from the system's folder picker) to [peerId]. */
    suspend fun sendFolder(peerId: String, tree: Uri): String? {
        val path = withContext(io) {
            val root = DocumentsContract.buildDocumentUriUsingTree(tree, DocumentsContract.getTreeDocumentId(tree))
            val (name, _) = describe(root)
            val folder = File(newFolder(), name)
            copyTree(tree, DocumentsContract.getTreeDocumentId(tree), folder)
            folder.path
        }
        return repository.send(peerId, listOf(path))
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

    private fun copyTree(tree: Uri, documentId: String, to: File) {
        to.mkdirs()
        val children = DocumentsContract.buildChildDocumentsUriUsingTree(tree, documentId)
        val columns = arrayOf(
            DocumentsContract.Document.COLUMN_DOCUMENT_ID,
            DocumentsContract.Document.COLUMN_DISPLAY_NAME,
            DocumentsContract.Document.COLUMN_MIME_TYPE,
            DocumentsContract.Document.COLUMN_LAST_MODIFIED,
        )
        context.contentResolver.query(children, columns, null, null, null)?.use { cursor ->
            while (cursor.moveToNext()) {
                val id = cursor.getString(0)
                val name = plainName(cursor.getString(1) ?: id)
                val modified = if (cursor.isNull(3)) null else cursor.getLong(3)
                val item = File(to, name)
                if (cursor.getString(2) == DocumentsContract.Document.MIME_TYPE_DIR) {
                    copyTree(tree, id, item)
                    modified?.takeIf { it > 0 }?.let(item::setLastModified)
                } else {
                    copy(DocumentsContract.buildDocumentUriUsingTree(tree, id), item, modified)
                }
            }
        }
    }

    /** A name from another app as one name here: no folders in it. */
    private fun plainName(name: String): String =
        name.replace('/', '_').replace('\u0000', '_').trim().ifEmpty { "item" }
}
