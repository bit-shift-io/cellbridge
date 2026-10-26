package dev.cellbridge

import android.app.PendingIntent
import android.content.Intent
import android.graphics.drawable.Icon
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.ResultReceiver
import android.service.quicksettings.Tile
import android.service.quicksettings.TileService
import dev.cellbridge.cellbridge.R
import org.json.JSONObject

internal data class TileStatus(val active: Boolean, val label: String)

/**
 * Maps the service snapshot to the Quick Settings tile. Error is treated as
 * active so a failed start can still be turned off from the tile, matching the
 * dashboard's toggle.
 */
internal fun tileStatus(snapshot: String): TileStatus {
    val state = try {
        JSONObject(snapshot).optString("state")
    } catch (_: Exception) {
        ""
    }
    return when (state) {
        "connected" -> TileStatus(true, "Connected")
        "connecting" -> TileStatus(true, "Connecting")
        "error" -> TileStatus(true, "Error")
        else -> TileStatus(false, "Idle")
    }
}

/** Quick Settings tile: the same start/stop commands as the dashboard. */
class BridgeTileService : TileService() {
    override fun onStartListening() {
        super.onStartListening()
        updateTile()
    }

    override fun onTileAdded() {
        super.onTileAdded()
        updateTile()
    }

    override fun onClick() {
        if (isLocked && Build.VERSION.SDK_INT >= 26) {
            unlockAndRun { toggle() }
        } else {
            toggle()
        }
    }

    private fun toggle() {
        val status = tileStatus(BridgeService.lastSnapshot)
        updateTile(TileStatus(!status.active, if (status.active) "Disconnecting" else "Connecting"))
        if (status.active) {
            val reply = object : ResultReceiver(Handler(Looper.getMainLooper())) {
                override fun onReceiveResult(code: Int, data: Bundle?) {
                    updateTile()
                }
            }
            try {
                BridgeService.command(applicationContext, BridgeService.STOP, reply)
            } catch (_: Exception) {
                updateTile()
            }
        } else {
            startBridgeActivity()
        }
    }

    /**
     * Android 14 rejects a foreground-service start made directly from a tile
     * click. Route the start through a transparent foreground activity instead.
     */
    private fun startBridgeActivity() {
        val intent = Intent(this, TileToggleActivity::class.java)
            .putExtra(TileToggleActivity.EXTRA_ACTION, BridgeService.START)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_NO_ANIMATION)
        if (Build.VERSION.SDK_INT >= 34) {
            startActivityAndCollapse(
                PendingIntent.getActivity(
                    this,
                    0,
                    intent,
                    PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
                ),
            )
        } else {
            @Suppress("DEPRECATION")
            startActivityAndCollapse(intent)
        }
    }

    private fun updateTile(status: TileStatus = tileStatus(BridgeService.lastSnapshot)) {
        val tile = qsTile ?: return
        tile.state = if (status.active) Tile.STATE_ACTIVE else Tile.STATE_INACTIVE
        tile.label = "CellBridge"
        tile.subtitle = status.label
        tile.icon = Icon.createWithResource(this, R.drawable.ic_cellbridge_tile)
        tile.updateTile()
    }
}
