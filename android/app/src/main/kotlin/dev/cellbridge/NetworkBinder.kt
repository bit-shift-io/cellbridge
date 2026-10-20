package dev.cellbridge

import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.os.Build
import android.os.ParcelFileDescriptor
import java.net.DatagramSocket

/**
 * The two transports the bridge depends on changing state.
 *
 * `up` is `true` when the transport's network appears ([NetworkBinder.onAvailable])
 * and `false` when it is lost ([NetworkBinder.onLost]). Callbacks are keyed on
 * the network's **transport**, never on a network identity, so a re-connected
 * radio that Android represents with a brand-new [Network] object still reports
 * `true`, and an unrelated network (a VPN, Ethernet) reports nothing at all.
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
    /** The listener for cellular/Wi-Fi changes. `null` drops the notifications. */
    var events: NetworkEvents? = null

    /** Bridges the framework callback to the transport-aware `onAvailable`/`onLost`. */
    private val callback = object : ConnectivityManager.NetworkCallback() {
        override fun onAvailable(network: Network) = this@NetworkBinder.onAvailable(network)

        override fun onLost(network: Network) = this@NetworkBinder.onLost(network)
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

    private var started = false

    /**
     * The DatagramSockets whose descriptor was handed to native code and that
     * Kotlin must therefore keep alive. See [bindWifiSocket].
     */
    private val retained = HashMap<Int, DatagramSocket>()

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
        cm.registerNetworkCallback(observedRequest, callback)
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
    fun bindProcess(cellular: Network) {
        cm.bindProcessToNetwork(cellular)
    }

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
     *   Rust's descriptor, so the socket is retained for the life of the process
     *   and deliberately never closed.
     */
    fun bindWifiSocket(network: Network): Int {
        val socket = DatagramSocket()
        // Pin before anything else can use the socket.
        network.bindSocket(socket)

        val fd = detachDescriptor(socket)
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
        notifyTransport(network, up = true)
    }

    /** A network of one of our transports was lost; reports it, ignoring the rest. */
    fun onLost(network: Network) {
        notifyTransport(network, up = false)
    }

    private fun networkUsing(transport: Int): Network? =
        cm.allNetworks.firstOrNull { network ->
            cm.getNetworkCapabilities(network)?.hasTransport(transport) == true
        }

    private fun notifyTransport(network: Network, up: Boolean) {
        // Transport, not identity: whatever object Android reports, the event is
        // decided by what the network is.
        val capabilities = cm.getNetworkCapabilities(network) ?: return
        val listener = events ?: return

        if (capabilities.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR)) {
            listener.onCellularChanged(up)
        }
        if (capabilities.hasTransport(NetworkCapabilities.TRANSPORT_WIFI)) {
            listener.onWifiChanged(up)
        }
        // Every other transport (VPN, Ethernet, ...) reports nothing.
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
