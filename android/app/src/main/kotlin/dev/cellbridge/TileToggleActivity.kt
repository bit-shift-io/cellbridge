package dev.cellbridge

import android.app.Activity
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.ResultReceiver

/**
 * Starts or stops the bridge from a foreground activity.
 *
 * Android 14 rejects `startForegroundService` from a Quick Settings tile click
 * (the tile is backgrounded), so the tile routes the start through this
 * transparent activity. It is invisible and finishes as soon as the service
 * command is issued.
 */
class TileToggleActivity : Activity() {
    private var handled = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        if (savedInstanceState != null) {
            finishAndRemoveTask()
        }
    }

    override fun onResume() {
        super.onResume()
        if (handled) return
        handled = true
        val action = intent.getStringExtra(EXTRA_ACTION) ?: BridgeService.START
        val reply = object : ResultReceiver(Handler(Looper.getMainLooper())) {
            override fun onReceiveResult(code: Int, data: Bundle?) {}
        }
        try {
            BridgeService.command(applicationContext, action, reply)
        } catch (_: Exception) {
        }
        finishAndRemoveTask()
    }

    companion object {
        const val EXTRA_ACTION = "dev.cellbridge.TILE_ACTION"
    }
}
