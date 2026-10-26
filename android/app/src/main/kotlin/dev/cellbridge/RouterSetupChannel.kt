package dev.cellbridge

import android.content.Context
import android.content.SharedPreferences
import android.net.ConnectivityManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.ParcelFileDescriptor
import android.util.Base64
import io.flutter.plugin.common.MethodCall
import io.flutter.plugin.common.MethodChannel
import org.json.JSONObject
import java.net.Inet4Address
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.Socket
import java.util.concurrent.Executors

const val ROUTER_SETUP_CHANNEL = "dev.cellbridge/router-setup"

/** Only phone identity and host pins may be persisted, never SSH credentials. */
class RouterSetupStore(private val prefs: SharedPreferences) {
    fun identity(): String? = prefs.getString("setup.phonePrivateKey", null)
    fun saveIdentity(key: String) {
        require(key.length == 44 && Base64.decode(key, Base64.NO_WRAP).size == 32) { "Invalid phone identity" }
        check(prefs.edit().putString("setup.phonePrivateKey", key).commit()) { "Could not save phone identity" }
    }
    fun pin(endpoint: String): String? = prefs.getString("setup.host.$endpoint", null)
    fun savePin(endpoint: String, fingerprint: String) {
        require(endpoint.isNotBlank() && endpoint.length <= 256)
        require(fingerprint.startsWith("SHA256:") && fingerprint.length <= 128)
        check(prefs.edit().putString("setup.host.$endpoint", fingerprint).commit()) { "Could not save router identity" }
    }
}

/** Excludes bridge startup/test operations without changing process binding. */
class RouterSetupLease : AutoCloseable {
    private var held = false
    @Synchronized fun acquire(): Boolean {
        if (held) return true
        held = BridgeController.tryBeginConnectionTest()
        return held
    }
    @Synchronized override fun close() {
        if (held) { held = false; BridgeController.endConnectionTest() }
    }
}

/**
 * The Wi-Fi link's IPv4 next-hop: the first route with an IPv4 gateway.
 * Connected routes carry a null gateway and are skipped, so this yields the
 * default gateway rather than the on-link network.
 */
internal fun firstIpv4Gateway(gateways: List<InetAddress?>): String? =
    gateways.firstOrNull { it is Inet4Address }?.hostAddress

/** Android owns network selection/storage; all SSH and scripts run in Rust. */
class RouterSetupChannel(
    context: Context,
    private val store: RouterSetupStore = RouterSetupStore(openEncryptedPreferences(context)),
) : MethodChannel.MethodCallHandler, AutoCloseable {
    private val cm = context.getSystemService(ConnectivityManager::class.java)
    private val worker = Executors.newSingleThreadExecutor()
    private val main = Handler(Looper.getMainLooper())
    private val lease = RouterSetupLease()
    private val lock = Any()
    private var generation = 0
    private var connecting = false
    private var closed = false
    private var socket: Socket? = null
    @Volatile private var native: NativeBridge? = null
    // Pre-API-29 ParcelFileDescriptor.fromSocket shares the socket's fd, so the
    // socket must outlive the handover or its finalizer closes Rust's fd.
    private val retained = java.util.concurrent.ConcurrentHashMap<Int, Socket>()

    override fun onMethodCall(call: MethodCall, result: MethodChannel.Result) {
        try {
            when (call.method) {
                "identity" -> result.success(store.identity())
                "saveIdentity" -> { store.saveIdentity(call.argument<String>("privateKey") ?: ""); result.success(null) }
                "hostPin" -> result.success(store.pin(call.argument<String>("endpoint") ?: ""))
                "saveHostPin" -> {
                    store.savePin(call.argument<String>("endpoint") ?: "", call.argument<String>("fingerprint") ?: "")
                    result.success(null)
                }
                "gateway" -> {
                    val wifi = NetworkBinder(cm).wifiNetwork()
                    val gateways = wifi?.let { cm.getLinkProperties(it)?.routes?.map { route -> route.gateway } }
                    result.success(firstIpv4Gateway(gateways ?: emptyList()) ?: "192.168.8.1")
                }
                "start" -> start(call, result)
                "snapshot" -> result.success(native?.routerSetupSnapshot() ?: "{\"state\":\"idle\",\"logs\":[]}")
                "cancel" -> { cancel(); result.success(null) }
                else -> result.notImplemented()
            }
        } catch (_: Exception) { result.error("router_setup", "Could not access router setup or encrypted identity", null) }
        catch (_: LinkageError) { result.error("router_setup", "Router setup is unavailable in this build", null) }
    }

    private fun start(call: MethodCall, result: MethodChannel.Result) {
        val host = call.argument<String>("host")?.trim().orEmpty()
        val port = call.argument<Number>("port")?.toInt() ?: 22
        val username = call.argument<String>("username").orEmpty()
        val password = call.argument<String>("password").orEmpty()
        val publicKey = call.argument<String>("phonePublicKey").orEmpty()
        val priority = call.argument<Number>("priority")?.toInt() ?: 0
        val fingerprint = call.argument<String>("expectedFingerprint")
        require(host.isNotEmpty() && host.length <= 253 && !host.any { it.isWhitespace() } && port in 1..65535)
        require(username.isNotBlank() && username.length <= 128 && password.length <= 4096 && priority in 1..10)
        require(publicKey.length == 44)
        val request = JSONObject().put("username", username).put("password", password)
            .put("phonePublicKey", publicKey).put("priority", priority)
            .put("expectedFingerprint", fingerprint ?: JSONObject.NULL).toString()
        val token = synchronized(lock) {
            check(!closed && !connecting) { "Router setup already connecting" }
            check(lease.acquire()) { "Disconnect the bridge before setup" }
            connecting = true
            ++generation
        }
        worker.execute {
            var active: Socket? = null
            var handedOff = false
            try {
                // Load native symbols before creating a transferable descriptor.
                val core = native ?: NativeBridge().also { native = it }
                val wifi = NetworkBinder(cm).wifiNetwork() ?: error("No Wi-Fi network")
                active = wifi.socketFactory.createSocket()
                synchronized(lock) {
                    check(generation == token && !closed)
                    socket = active
                }
                val address = wifi.getAllByName(host).firstOrNull { it is Inet4Address } ?: error("No IPv4 router address")
                active.connect(InetSocketAddress(address, port), 15_000)
                synchronized(lock) {
                    check(generation == token && !closed)
                    // dup() gives Rust an independent descriptor on every API
                    // level. pfd.close() closes only the PFD's own descriptor:
                    // on >=Q that is a duplicate (so the socket is closed
                    // separately); on <Q it is the socket's descriptor, so the
                    // socket is retained and never closed again.
                    val pfd = ParcelFileDescriptor.fromSocket(active)
                    val fd = pfd.dup().detachFd()
                    pfd.close()
                    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                        active.close()
                    } else {
                        retained[fd] = active
                    }
                    handedOff = true
                    core.routerSetup(request, fd)
                }
                main.post { result.success(null) }
            } catch (_: Exception) {
                main.post { result.error("router_setup", "SSH connection failed. Check Wi-Fi, router address and port, then retry.", null) }
            } catch (_: LinkageError) {
                main.post { result.error("router_setup", "Router setup native support is unavailable", null) }
            } finally {
                // Only close the socket if its descriptor was never handed to
                // Rust; otherwise ownership (and the single close) is Rust's or
                // the retained-socket path above.
                if (!handedOff) { try { active?.close() } catch (_: Exception) {} }
                synchronized(lock) { if (generation == token) { socket = null; connecting = false } }
            }
        }
    }

    private fun cancel() = synchronized(lock) {
        generation++
        connecting = false
        try { socket?.close() } catch (_: Exception) {}
        socket = null
        try { native?.cancelRouterSetup() } catch (_: LinkageError) {} finally { lease.close() }
    }

    override fun close() {
        synchronized(lock) { closed = true }
        try { cancel() } catch (_: Throwable) {}
        worker.shutdownNow()
    }
}
