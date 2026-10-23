package dev.cellbridge

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

class BootReceiver(private val store: (Context) -> ConfigStore = { EncryptedConfigStore(it) }) : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != Intent.ACTION_BOOT_COMPLETED) return
        try {
            if (store(context).read() != null) BridgeService.start(context)
        } catch (_: Exception) {
            android.util.Log.e("CellBridge", "Boot configuration unavailable")
        }
    }
}
