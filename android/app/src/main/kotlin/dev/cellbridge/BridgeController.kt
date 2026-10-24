package dev.cellbridge

import android.net.Network
import android.util.Log
import org.json.JSONArray
import org.json.JSONObject

data class NetworkSelection(val cellular: Network, val wifi: Network, val testOverride: Boolean = false)

interface BridgeNetworks {
    fun start(changed: () -> Unit)
    fun current(): NetworkSelection?
    fun bindCellular(network: Network): Boolean
    fun wifiSocket(network: Network): Int
    fun unbind()
    fun stop()
    fun cellularControl(network: Network): String
}

/** All methods run on the service's single worker, never an Activity thread. */
class BridgeController(
    private val networks: BridgeNetworks,
    private val bridge: BridgeFacade,
    private val dispatch: (() -> Unit) -> Unit,
) {
    private var requested = false
    private var running = false
    private var config: Map<String, Any?> = emptyMap()
    private var selected: NetworkSelection? = null
    private var error: String? = null
    private val events = ArrayDeque<JSONObject>()

    fun start(config: Map<String, Any?>): Unit = synchronized(ownershipLock) {
        if (requested) return
        if (connectionTestRunning) {
            reportStartupFailure("Connection test in progress; try connecting again when it finishes")
            return
        }
        owner?.stop()
        owner = this
        requested = true
        this.config = config
        error = null
        try {
            networks.start { dispatch { refresh() } }
            refresh()
        } catch (_: Exception) {
            error = "Could not request Android networks"
            event(error!!)
        }
    }

    fun startFromStore(store: ConfigStore) {
        if (requested) return
        val saved = try { store.read() } catch (_: Exception) {
            reportStartupFailure("Could not load encrypted bridge configuration")
            return
        }
        if (saved == null) reportStartupFailure("No saved bridge configuration")
        else start(saved)
    }

    fun reportStartupFailure(message: String) {
        error = message
        event(message)
    }

    fun refresh(): Unit = synchronized(ownershipLock) {
        if (!requested || owner !== this) return
        val next = networks.current()
        if (next == selected && running) return
        releaseSession()
        if (next == null) {
            error = "Waiting for cellular and Wi-Fi networks"
            event(error!!); return
        }
        try {
            if (!networks.bindCellular(next.cellular)) {
                networks.unbind()
                error = "Cellular process binding failed; forwarding stopped"
                event(error!!); return
            }
            val configJson = JSONObject(config).toString()
            val fd = networks.wifiSocket(next.wifi)
            // Native library is loaded when the facade is constructed, before
            // creating any transferable descriptor.
            bridge.start(configJson, fd)
            selected = next
            running = true
            error = null
            if (next.testOverride) event("TEST MODE: Wi-Fi egress, cellular NOT verified; Wi-Fi socket bound: ${next.wifi}")
            else event("Cellular binding verified: ${next.cellular}; Wi-Fi socket bound: ${next.wifi}")
        } catch (failure: Exception) {
            networks.unbind()
            val known = setOf("Invalid phone private key", "Invalid peer public key", "Invalid preshared key",
                "Invalid peer endpoint", "Invalid tunnel address", "Invalid bridge configuration", "Bridge is already running")
            error = failure.message?.takeIf { it in known } ?: "Could not start native bridge"
            event(error!!)
        }
    }

    private fun releaseSession() {
        if (running) {
            // If teardown fails, do not remove the binding underneath live flows.
            bridge.stop()
            running = false
            event("Native flows stopped")
        }
        selected = null
        networks.unbind()
    }

    fun stop(): Unit = synchronized(ownershipLock) {
        requested = false
        if (owner === this) {
            releaseSession()
            owner = null
        }
        networks.stop()
        error = null
    }

    fun cellularControl(): String {
        val current = selected ?: throw IllegalStateException("No verified cellular binding")
        check(!current.testOverride) { "No verified cellular binding" }
        return networks.cellularControl(current.cellular)
    }

    fun snapshot(): JSONObject {
        val result = if (running) JSONObject(bridge.snapshotJson()) else JSONObject()
            .put("state", if (requested || error != null) "error" else "idle")
            .put("error", error ?: JSONObject.NULL)
            .put("txBytes", "0").put("rxBytes", "0").put("activeFlows", "0")
        result.put("binding", JSONObject()
            .put("cellularBindVerified", running && selected?.testOverride != true)
            .put("egressTestOverride", running && selected?.testOverride == true)
            .put("cellularNetwork", selected?.takeUnless { it.testOverride }?.cellular?.toString() ?: JSONObject.NULL)
            .put("wifiNetwork", selected?.wifi?.toString() ?: JSONObject.NULL))
        val diagnostics = result.optJSONArray("diagnostics") ?: JSONArray()
        for (event in events) diagnostics.put(event)
        result.put("diagnostics", diagnostics)
        result.put("requested", requested)
        if (error != null) result.put("state", "error").put("error", error)
        return result
    }

    private fun event(message: String) {
        if (events.lastOrNull()?.optString("message") == message) return
        if (events.size == 256) events.removeFirst()
        events.addLast(JSONObject().put("timestampMs", System.currentTimeMillis()).put("component", "android").put("message", message))
        Log.i("CellBridge", message)
    }

    companion object {
        // Process binding and the Rust lifecycle are process-global, not scoped
        // to a Service object. Retire the old owner before a replacement binds.
        private val ownershipLock = Any()
        private var owner: BridgeController? = null
        private var connectionTestRunning = false

        fun tryBeginConnectionTest(): Boolean = synchronized(ownershipLock) {
            if (owner?.requested == true || connectionTestRunning) false
            else { connectionTestRunning = true; true }
        }

        fun endConnectionTest(): Unit = synchronized(ownershipLock) {
            connectionTestRunning = false
        }
    }
}
