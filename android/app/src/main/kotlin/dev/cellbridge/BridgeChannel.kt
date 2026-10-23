package dev.cellbridge

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.content.pm.PackageManager
import android.os.*
import android.provider.Settings
import io.flutter.plugin.common.MethodCall
import io.flutter.plugin.common.MethodChannel

const val BRIDGE_CHANNEL = "dev.cellbridge/bridge"

/** UI adapter only: the service owns everything after Activity destruction. */
class BridgeChannel(private val activity: Activity) : MethodChannel.MethodCallHandler {
    override fun onMethodCall(call: MethodCall, result: MethodChannel.Result) {
        when (call.method) {
            "snapshot" -> result.success(BridgeService.lastSnapshot)
            "batterySettings" -> {
                activity.startActivity(Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS))
                result.success(null)
            }
            "start", "stop", "cellularControl" -> {
                if (call.method == "start" && Build.VERSION.SDK_INT >= 33 && activity.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
                    activity.requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 51820)
                }
                val action = when (call.method) { "start" -> BridgeService.START; "stop" -> BridgeService.STOP; else -> BridgeService.CONTROL }
                val receiver = object : ResultReceiver(Handler(Looper.getMainLooper())) {
                    override fun onReceiveResult(code: Int, data: Bundle?) {
                        if (code != 0) result.error("bridge", "Native service operation failed; check diagnostics", null)
                        else result.success(data?.getString(if (action == BridgeService.CONTROL) "address" else "snapshot"))
                    }
                }
                try { BridgeService.command(activity.applicationContext, action, receiver) }
                catch (_: Exception) { result.error("bridge", "Android could not start the bridge service", null) }
            }
            else -> result.notImplemented()
        }
    }
}
