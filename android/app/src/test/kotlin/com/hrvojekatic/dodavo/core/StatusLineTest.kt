package com.hrvojekatic.dodavo.core

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Test

/** What the status line says when the same thing is said again, and when something newer comes meanwhile. */
@OptIn(ExperimentalCoroutinesApi::class)
class StatusLineTest {
    /** A core that is never started: the line needs none. */
    private object NoCore : DodavoCore {
        override fun start(options: StartOptions): Started = error("not started here")
        override fun call(call: Call): String = "null"
        override fun next(timeoutMs: Long): Message? = null
    }

    private val told = "No app on this device can share it."

    @Test
    fun theSameTextIsReadAgain() = runTest {
        val repository = CoreRepository(NoCore, backgroundScope)
        repository.tell(told)
        assertEquals(told, repository.line.value)
        // A live region is read only when it changes: empty for a moment, then the text again.
        repository.tell(told)
        assertEquals("", repository.line.value)
        advanceTimeBy(SAID_AGAIN_AFTER_MS)
        runCurrent()
        assertEquals(told, repository.line.value)
        assertEquals(2L, repository.announcement.value?.serial)
    }

    @Test
    fun somethingNewerIsNotTakenBack() = runTest {
        val repository = CoreRepository(NoCore, backgroundScope)
        repository.tell(told)
        repository.tell(told)
        repository.handle(Message.Status(online = true, text = "Online"))
        advanceTimeBy(SAID_AGAIN_AFTER_MS)
        runCurrent()
        assertEquals("Online", repository.line.value)
    }

    @Test
    fun theSameStatusIsNotReadAgain() = runTest {
        val repository = CoreRepository(NoCore, backgroundScope)
        repository.handle(Message.Status(online = true, text = "Online"))
        repository.handle(Message.Status(online = true, text = "Online"))
        assertEquals("Online", repository.line.value)
    }
}
