package dev.cellbridge

import android.app.*
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.net.ConnectivityManager
import android.os.*
import android.service.quicksettings.TileService
import org.json.JSONObject

/**
 * The ongoing notification's title is the connection status itself, so the
 * shade reads "Connected" or "Disconnected" rather than the app name.
 */
internal fun notificationTitle(status: String): String =
    if (status == "connected") "Connected" else "Disconnected"

/** Native service owns networking. No Flutter engine is created here. */
class BridgeService : Service() {
    private lateinit var thread: HandlerThread
    private lateinit var worker: Handler
    private var controller: BridgeController? = null
    private var wakeLock: PowerManager.WakeLock? = null
    private var lastWakeRenewal = 0L
    private var destroyed = false

    override fun onCreate() {
        super.onCreate()
        val manager = getSystemService(NotificationManager::class.java)
        if (Build.VERSION.SDK_INT >= 26) manager.createNotificationChannel(NotificationChannel(CHANNEL, "CellBridge", NotificationManager.IMPORTANCE_LOW))
        startForeground(NOTIFICATION, notification("Starting bridge"))
        thread = HandlerThread("CellBridge-service").also { it.start() }
        worker = Handler(thread.looper)
        synchronized(serviceStateLock) { instance = this }
        worker.post(tick)
    }

    private val tick = object : Runnable {
        override fun run() {
            if (destroyed) return
            try {
                controller?.refresh()
                publish()
            } catch (_: Exception) { fail("Native bridge recovery failed") }
            worker.postDelayed(this, 1000)
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        @Suppress("DEPRECATION")
        val reply = intent?.getParcelableExtra<ResultReceiver>("reply")
        val action = intent?.action ?: START
        worker.post {
            try {
                when (action) {
                    STOP -> {
                        controller?.stop()
                        publish()
                        reply?.send(0, Bundle().apply { putString("snapshot", lastSnapshot) })
                        stopSelf(startId)
                        return@post
                    }
                    CONTROL -> {
                        val address = controller?.cellularControl() ?: throw IllegalStateException()
                        reply?.send(0, Bundle().apply { putString("address", address) })
                        return@post
                    }
                    else -> {
                        if (controller == null) {
                            val network = AndroidBridgeNetworks(getSystemService(ConnectivityManager::class.java)) {
                                EgressTestOverride.enabled(dev.cellbridge.cellbridge.BuildConfig.DEBUG, contentResolver)
                            }
                            controller = BridgeController(network, NativeBridge()) { action -> worker.post(action) }
                        }
                        controller!!.startFromStore(EncryptedConfigStore(applicationContext))
                        publish()
                    }
                }
                reply?.send(0, Bundle().apply { putString("snapshot", lastSnapshot) })
            } catch (_: LinkageError) {
                fail("Native bridge library is unavailable")
                reply?.send(1, Bundle())
                stopSelf(startId)
            } catch (_: Exception) {
                fail(if (action == CONTROL) "Cellular control request failed" else "Could not start or stop bridge")
                reply?.send(1, Bundle())
            }
        }
        return START_STICKY
    }

    private fun publish() {
        val snapshot = controller?.snapshot() ?: return
        synchronized(serviceStateLock) { if (instance === this) lastSnapshot = snapshot.toString() }
        val connected = snapshot.optString("state") == "connected"
        if (connected) {
            if (wakeLock == null) wakeLock = getSystemService(PowerManager::class.java)
                .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "CellBridge:forwarding")
                .also { it.setReferenceCounted(false) }
            val now = SystemClock.elapsedRealtime()
            if (wakeLock?.isHeld != true || now - lastWakeRenewal >= 300_000) {
                wakeLock!!.acquire(600_000L)
                lastWakeRenewal = now
            }
        } else if (!connected) releaseWakeLock()
        getSystemService(NotificationManager::class.java).notify(NOTIFICATION, notification(snapshot.optString("state")))
        notifyTile()
    }

    private fun notifyTile() {
        try {
            TileService.requestListeningState(this, ComponentName(this, BridgeTileService::class.java))
        } catch (_: Exception) {
        }
    }

    private fun fail(message: String) {
        controller?.reportStartupFailure(message)
        synchronized(serviceStateLock) {
            if (instance === this) lastSnapshot = JSONObject(idleSnapshot()).put("state", "error").put("error", message).toString()
        }
        notifyTile()
        android.util.Log.e("CellBridge", message)
    }

    private fun releaseWakeLock() { wakeLock?.let { if (it.isHeld) it.release() }; wakeLock = null }

    private fun notification(status: String): Notification {
        val stop = PendingIntent.getService(this, 1, Intent(this, BridgeService::class.java).setAction(STOP), PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
        val open = PendingIntent.getActivity(this, 0, packageManager.getLaunchIntentForPackage(packageName), PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
        @Suppress("DEPRECATION")
        val builder = if (Build.VERSION.SDK_INT >= 26) Notification.Builder(this, CHANNEL) else Notification.Builder(this)
        // The status is the title; there is no longer a "CellBridge" title or
        // a content-text subtitle.
        return builder
            .setSmallIcon(android.R.drawable.stat_sys_upload_done)
            .setContentTitle(notificationTitle(status)).setOngoing(true)
            .setContentIntent(open).addAction(Notification.Action.Builder(null, "Disconnect", stop).build()).build()
    }

    override fun onDestroy() {
        synchronized(serviceStateLock) { if (instance === this) instance = null }
        worker.post {
            destroyed = true
            worker.removeCallbacks(tick)
            try {
                controller?.stop()
                synchronized(serviceStateLock) {
                    if (instance == null && JSONObject(lastSnapshot).optString("state") != "error") lastSnapshot = idleSnapshot()
                }
                notifyTile()
            }
            finally { releaseWakeLock(); thread.quitSafely() }
        }
        super.onDestroy()
    }
    override fun onBind(intent: Intent?): IBinder? = null

    companion object {
        const val START = "dev.cellbridge.START"
        const val STOP = "dev.cellbridge.STOP"
        const val CONTROL = "dev.cellbridge.CONTROL"
        private const val CHANNEL = "cellbridge"
        private const val NOTIFICATION = 51820
        private val serviceStateLock = Any()
        @Volatile private var instance: BridgeService? = null
        @Volatile var lastSnapshot: String = idleSnapshot()
            private set
        private fun idleSnapshot() = "{\"state\":\"idle\",\"error\":null,\"txBytes\":\"0\",\"rxBytes\":\"0\",\"activeFlows\":\"0\",\"diagnostics\":[],\"binding\":{\"cellularBindVerified\":false}}"

        fun start(context: Context) {
            val intent = Intent(context, BridgeService::class.java).setAction(START)
            if (Build.VERSION.SDK_INT >= 26) context.startForegroundService(intent) else context.startService(intent)
        }
        fun command(context: Context, action: String, reply: ResultReceiver) {
            if (action == STOP && instance == null) {
                reply.send(0, Bundle().apply { putString("snapshot", lastSnapshot) }); return
            }
            if (action == CONTROL && instance == null) { reply.send(1, Bundle()); return }
            val intent = Intent(context, BridgeService::class.java).setAction(action).putExtra("reply", reply)
            if (action == START && Build.VERSION.SDK_INT >= 26) context.startForegroundService(intent) else context.startService(intent)
        }
    }
}
