package dev.cellbridge

import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.os.Build
import android.os.ParcelFileDescriptor
import java.net.DatagramSocket
import java.util.concurrent.ConcurrentHashMap

/**
 * The two transports the bridge depends on changing state.
 *
 * `up` is `true` when the transport's network appears ([NetworkBinder.onAvailable])
 * and `false` when it is lost ([NetworkBinder.onLost]).
 *
 * Which callback fires is decided by the **transport** the network carries, never
 * by a network identity, so a re-connected radio that Android represents with a
 * brand-new [Network] object still reports `true`, and an unrelated network (a
 * VPN, Ethernet) reports nothing at all. Network identity is used *only* to
 * correlate an `onLost` with the `onAvailable` that preceded it; see
 * [NetworkBinder.onLost].
 */
interface NetworkEvents {
    /** The cellular (internet-egress) side came up (`true`) or went down (`false`). */
    fun onCellularChanged(up: Boolean)

    /** The Wi-Fi (WireGuard-faces-the-router) side came up (`true`) or went down (`false`). */
    fun onWifiChanged(up: Boolean)
}

/**
 * Steers the bridge's sockets onto the right network without root.
 *
 * Rootless Android cannot use `SO_BINDTODEVICE`, so the platform layer has to
 * do the pinning (D5 in the design):
 *
 * - [bindProcess] sends every socket Rust opens afterwards out over **cellular**,
 *   so the proxy's TCP/UDP egress is the phone's mobile internet.
 * - [bindWifiSocket] pins the one WireGuard socket to **Wi-Fi**, so the tunnel
 *   itself travels over the LAN to the router rather than back out cellular.
 *
 * # Lifecycle
 *
 * Registration is explicit and idempotent: [start] registers the callbacks and
 * asks Android to keep a cellular network provisioned (it may otherwise be torn
 * down while Wi-Fi is the default network), and [stop] unregisters them. Calling
 * either twice is a no-op, so no callback is ever leaked. Task 10's foreground
 * service owns this lifecycle; this class owns nothing that outlives it.
 *
 * A [NetworkEvents] listener may be attached at any time via [events]; before one
 * is attached the callbacks are simply dropped.
 *
 * @param cm the system `ConnectivityManager`.
 * @param detachDescriptor how a bound socket's descriptor is detached for
 *   handover to native code. Production uses [detachDatagramDescriptor]; this is
 *   an injection point only because Robolectric cannot produce a real
 *   `DatagramSocket` descriptor (see the tests).
 */
class NetworkBinder(
    private val cm: ConnectivityManager,
    private val detachDescriptor: (DatagramSocket) -> Int = ::detachDatagramDescriptor,
) {
    /**
     * The listener for cellular/Wi-Fi changes. `null` drops the notifications.
     *
     * Written by the owner's thread and read from the connectivity handler
     * thread, hence `@Volatile` so the hand-off is explicit.
     */
    @Volatile
    var events: NetworkEvents? = null

    /**
     * The transports each currently-available network carries, keyed by the exact
     * [Network] object Android reported in `onAvailable`.
     *
     * The framework tears a network down *before* it delivers `onLost`, so by then
     * `getNetworkCapabilities()` is already `null` and cannot be re-queried. This
     * record is what `onLost` consults instead. Identity only correlates the down
     * event with the up event that preceded it; which callback fires is still
     * decided by the transport captured while the network was alive.
     */
    private val transportsByNetwork = HashMap<Network, Set<Int>>()

    /** Bridges the framework callback to the transport-aware `onAvailable`/`onLost`. */
    private val callback = object : ConnectivityManager.NetworkCallback() {
        override fun onAvailable(network: Network) = this@NetworkBinder.onAvailable(network)

        override fun onLost(network: Network) = this@NetworkBinder.onLost(network)

        override fun onCapabilitiesChanged(
            network: Network,
            capabilities: NetworkCapabilities,
        ) = this@NetworkBinder.onCapabilitiesChanged(network, capabilities)
    }

    /**
     * Keeps a cellular network up while Wi-Fi is the default. The resulting
     * up/down transitions are observed by [callback]; this one needs no handlers.
     */
    private val cellularKeeper = object : ConnectivityManager.NetworkCallback() {}

    /** Matches only cellular, so Android provisions it and holds it. */
    private val cellularRequest: NetworkRequest =
        NetworkRequest.Builder()
            .addTransportType(NetworkCapabilities.TRANSPORT_CELLULAR)
            .addCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
            .build()

    /**
     * Matches either transport of interest (transport requirements are OR-ed).
     * A network with no capability requirement, so a Wi-Fi link Android flags
     * "no internet" still reports its transitions.
     */
    private val observedRequest: NetworkRequest =
        NetworkRequest.Builder()
            .addTransportType(NetworkCapabilities.TRANSPORT_CELLULAR)
            .addTransportType(NetworkCapabilities.TRANSPORT_WIFI)
            .build()

    /**
     * Whether [start] has both registered. Written and read from the owner's
     * thread, but marked `@Volatile` so re-entry and [stop] always see the final
     * value.
     */
    @Volatile
    private var started = false

    /**
     * The DatagramSockets whose descriptor was handed to native code and that
     * Kotlin must therefore keep alive. See [bindWifiSocket].
     *
     * Written only from [bindWifiSocket], but the caller may be on any thread, so
     * a concurrent map keeps that contract explicit.
     */
    private val retained = ConcurrentHashMap<Int, DatagramSocket>()

    /**
     * Begin observing the networks and holding cellular up. Idempotent.
     *
     * Must be paired with [stop]; Task 10's service drives both.
     */
    fun start() {
        if (started) {
            return
        }
        cm.requestNetwork(cellularRequest, cellularKeeper)
        try {
            cm.registerNetworkCallback(observedRequest, callback)
        } catch (e: RuntimeException) {
            // The cellular request landed but observation did not start: release
            // the keeper so a retry cannot leak it. `started` stays false, so a
            // retry runs the whole sequence again.
            cm.unregisterNetworkCallback(cellularKeeper)
            throw e
        }
        started = true
    }

    /** Stop observing and release the cellular request. Idempotent. */
    fun stop() {
        if (!started) {
            return
        }
        cm.unregisterNetworkCallback(callback)
        cm.unregisterNetworkCallback(cellularKeeper)
        started = false
    }

    /**
     * The current cellular network, or `null` when there is none.
     *
     * Never throws and never returns a non-cellular network.
     */
    fun cellularNetwork(): Network? = networkUsing(NetworkCapabilities.TRANSPORT_CELLULAR)

    /**
     * The current Wi-Fi network, or `null` when there is none.
     *
     * Never throws and never returns a non-Wi-Fi network.
     */
    fun wifiNetwork(): Network? = networkUsing(NetworkCapabilities.TRANSPORT_WIFI)

    /**
     * Bind the process to [cellular], so sockets Rust opens afterwards default to
     * the cellular network. Call this before the bridge starts.
     */
    fun bindProcess(cellular: Network): Boolean = cm.bindProcessToNetwork(cellular)

    /**
     * Create a UDP socket bound to [network] (Wi-Fi) and hand its descriptor to
     * native code, returning the raw fd.
     *
     * # Ownership contract
     *
     * Rust wraps the returned descriptor with `UdpSocket::from_raw_fd` and takes
     * ownership; it closes the descriptor exactly once, when the WireGuard socket
     * is dropped. Kotlin therefore hands over **exactly one** descriptor and must
     * never close it or let anything else close it.
     *
     * The trap this avoids: a bare `DatagramSocket` owns its descriptor, and if
     * the socket becomes unreachable its finalizer may close that descriptor out
     * from under the native code — an intermittent, load-dependent failure that
     * looks like a WireGuard bug. So the descriptor handed over is a *detached*
     * one (never owned by a [ParcelFileDescriptor] again), and the socket is
     * neutralised:
     *
     * - API 29+: `ParcelFileDescriptor.fromDatagramSocket` duplicates the
     *   descriptor, so the socket's own descriptor is independent of Rust's. The
     *   socket is closed here, which keeps the handover leak-free.
     * - API 24-28: `fromDatagramSocket` does **not** duplicate and instead shares
     *   the socket's descriptor. Closing (or finalizing) the socket would close
     *   Rust's descriptor, so the socket is retained for the life of this binder
     *   and deliberately never closed.
     */
    fun bindWifiSocket(network: Network): Int {
        val socket = DatagramSocket()
        // Pin before anything else can use the socket.
        val fd = try {
            network.bindSocket(socket)
            detachDescriptor(socket)
        } catch (e: Exception) {
            socket.close()
            throw e
        }
        // Ownership of `fd` now belongs to Rust.
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            socket.close()
        } else {
            retained[fd] = socket
        }
        return fd
    }

    /** A network of one of our transports came up; reports it, ignoring the rest. */
    fun onAvailable(network: Network) {
        // Capture the transport now, while the network is still alive; `onLost`
        // cannot re-query it later.
        val capabilities = cm.getNetworkCapabilities(network) ?: return
        val transports = transportsIn(capabilities)
        transportsByNetwork[network] = transports
        report(transports, up = true)
    }

    /**
     * Refresh the recorded transports for a network we already know about.
     *
     * Registered so the record tracks capability changes for the life of a
     * network. A network we never saw available (and so have no reason to report
     * on) is ignored.
     */
    fun onCapabilitiesChanged(network: Network, capabilities: NetworkCapabilities) {
        if (transportsByNetwork.containsKey(network)) {
            transportsByNetwork[network] = transportsIn(capabilities)
        }
    }

    /**
     * A network of one of our transports was lost; reports it, ignoring the rest.
     *
     * Deliberately does **not** call `getNetworkCapabilities()`: the framework has
     * already removed the network by the time `onLost` is delivered, so that call
     * returns `null`. Instead the transport recorded when the network came up is
     * used, then pruned. Identity is the correlation key here, not a way to
     * identify the transport — that still comes from the recorded capabilities.
     */
    fun onLost(network: Network) {
        val transports = transportsByNetwork.remove(network) ?: return
        report(transports, up = false)
    }

    /**
     * Scan every network for one carrying [transport].
     *
     * `getAllNetworks` is deprecated as of API 31 with no non-deprecated drop-in
     * for enumerating every network, so the deprecation is suppressed here.
     */
    @Suppress("DEPRECATION")
    private fun networkUsing(transport: Int): Network? =
        cm.allNetworks.firstOrNull { network ->
            val caps = cm.getNetworkCapabilities(network)
            caps?.hasTransport(transport) == true && !caps.hasTransport(NetworkCapabilities.TRANSPORT_VPN) &&
                (transport != NetworkCapabilities.TRANSPORT_CELLULAR || caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET))
        }

    /** Fire the listener for each [transports] entry we report on. */
    private fun report(transports: Set<Int>, up: Boolean) {
        val listener = events ?: return
        if (NetworkCapabilities.TRANSPORT_CELLULAR in transports) {
            listener.onCellularChanged(up)
        }
        if (NetworkCapabilities.TRANSPORT_WIFI in transports) {
            listener.onWifiChanged(up)
        }
        // Every other transport (VPN, Ethernet, ...) reports nothing.
    }

    /** The subset of our transports that [capabilities] carries. */
    private fun transportsIn(capabilities: NetworkCapabilities): Set<Int> = buildSet {
        if (capabilities.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR)) {
            add(NetworkCapabilities.TRANSPORT_CELLULAR)
        }
        if (capabilities.hasTransport(NetworkCapabilities.TRANSPORT_WIFI)) {
            add(NetworkCapabilities.TRANSPORT_WIFI)
        }
    }
}

/**
 * Detach a socket's descriptor for handover to native code.
 *
 * `detachFd` returns the descriptor and gives up ownership of it, so the caller
 * (Rust) becomes responsible for closing it exactly once.
 */
private fun detachDatagramDescriptor(socket: DatagramSocket): Int =
    ParcelFileDescriptor.fromDatagramSocket(socket).detachFd()
