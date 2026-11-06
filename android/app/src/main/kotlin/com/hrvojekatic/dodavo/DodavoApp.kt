package com.hrvojekatic.dodavo

import android.app.Application

/** Starts Dodavo's core with the process, whatever part of the app runs first. */
class DodavoApp : Application() {
    lateinit var container: AppContainer
        private set

    override fun onCreate() {
        super.onCreate()
        container = AppContainer(this)
        container.repository.start(container.startOptions())
        container.keepAlive.start()
    }
}
