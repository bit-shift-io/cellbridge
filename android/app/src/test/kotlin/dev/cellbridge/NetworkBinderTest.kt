package dev.cellbridge

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkInfo
import android.os.Build
import java.lang.ref.WeakReference
import java.net.DatagramSocket
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.shadows.ShadowNetwork
import org.robolectric.shadows.ShadowNetworkCapabilities
import org.robolectric.shadows.ShadowNetworkInfo

/**
 * The Android platform layer is the only part of CellBridge an unrooted Rust
 * process cannot do for itself: choosing the cellular (internet egress) and
 * Wi-Fi (WireGuard-to-router) networks and pinning sockets to them.
 *
 * Everything here runs under Robolectric. The one thing Robolectric cannot do
 * is hand out a real descriptor for a `DatagramSocket` — `ParcelFileDescriptor
 * .fromDatagramSocket` needs the hidden `DatagramSocket.getFileDescriptor$()`.
 * Those tests therefore verify the *ownership* contract through an injected
 * handover, not a real fd.
 *
 * Robolectric also cannot fail a `registerNetworkCallback` on demand, so the
 * `start()` failure-cleanup path has no unit test here (see the report).
 */
// The Robolectric SDK is pinned rather than left to the harness default: without
// `unitTests.isIncludeAndroidResources` Robolectric cannot read the manifest and
// falls back to API 23 (below our minSdk). API 36 cannot initialise its
// `ApplicationSharedMemory` on the JDK 21 this project builds with, so 35 is the
// newest SDK the suite can run on.
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class NetworkBinderTest {

    @Test fun debug_override_allows_wifi_only_without_process_binding_and_recovers_when_disabled() {
        shadowOf(cm).removeNetwork(cellular)
        var enabled = true
        val networks = AndroidBridgeNetworks(cm) { enabled }
        val selection = networks.current()!!
        assertTrue(selection.testOverride)
        assertSame(wifi, selection.wifi)
        assertTrue(networks.bindCellular(selection.cellular))
        assertNull(cm.boundNetworkForProcess)
        enabled = false
        assertNull(networks.current())
        assertFalse(networks.bindCellular(wifi))
    }

    @Test fun debug_override_never_replaces_available_cellular() {
        val networks = AndroidBridgeNetworks(cm) { true }
        val selection = networks.current()!!
        assertFalse(selection.testOverride)
        assertSame(cellular, selection.cellular)
    }

    @Test fun release_ignores_adb_setting_even_when_enabled() {
        val resolver = RuntimeEnvironment.getApplication().contentResolver
        android.provider.Settings.Global.putInt(resolver, "cellbridge_egress_test", 1)
        assertFalse(EgressTestOverride.enabled(false, resolver))
        assertTrue(EgressTestOverride.enabled(true, resolver))
        android.provider.Settings.Global.putInt(resolver, "cellbridge_egress_test", 0)
        assertFalse(EgressTestOverride.enabled(true, resolver))
    }

    private lateinit var cm: ConnectivityManager
    private lateinit var wifi: Network
    private lateinit var cellular: Network
    private lateinit var binder: NetworkBinder

    @Before
    fun setUp() {
        val context = RuntimeEnvironment.getApplication()
        cm = context.getSystemService(Context.CONNECTIVITY_SERVICE) as ConnectivityManager
        // Drop the shadow's default Wi-Fi/mobile networks so each test owns the
        // exact set it asserts against.
        shadowOf(cm).clearAllNetworks()
        wifi = addNetwork(NET_ID_WIFI, NetworkCapabilities.TRANSPORT_WIFI)
        cellular = addNetwork(NET_ID_CELLULAR, NetworkCapabilities.TRANSPORT_CELLULAR)
        binder = NetworkBinder(cm)
    }

    @After
    fun tearDown() {
        binder.stop()
    }

    @Test
    fun cellularNetwork_selects_the_cellular_network() {
        assertSame(cellular, binder.cellularNetwork())
    }

    @Test
    fun keeper_requests_internet_and_selection_rejects_noninternet_cellular_network() {
        val field = NetworkBinder::class.java.getDeclaredField("cellularRequest").apply { isAccessible = true }
        val request = field.get(binder) as android.net.NetworkRequest
        assertTrue(request.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET))
        val caps = cm.getNetworkCapabilities(cellular)!!
        shadowOf(caps).removeCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
        shadowOf(cm).setNetworkCapabilities(cellular, caps)
        assertNull(binder.cellularNetwork())
        assertSame(wifi, binder.wifiNetwork()) // LAN Wi-Fi need not have internet.
    }

    @Test
    fun wifiNetwork_selects_the_wifi_network() {
        assertSame(wifi, binder.wifiNetwork())
    }

    @Test
    fun cellularNetwork_is_null_without_a_cellular_network() {
        shadowOf(cm).removeNetwork(cellular)

        assertNull("no cellular network must be null, never Wi-Fi", binder.cellularNetwork())
        assertSame(wifi, binder.wifiNetwork())
    }

    @Test
    fun wifiNetwork_is_null_without_a_wifi_network() {
        shadowOf(cm).removeNetwork(wifi)

        assertNull(binder.wifiNetwork())
        assertSame(cellular, binder.cellularNetwork())
    }

    @Test
    fun bindProcess_binds_the_process_to_the_cellular_network() {
        binder.bindProcess(cellular)

        assertSame(cellular, cm.boundNetworkForProcess)
    }

    @Test
    fun onAvailable_reports_the_cellular_transport() {
        val recorder = Recorder()
        binder.events = recorder

        binder.onAvailable(cellular)

        assertEquals(listOf("cellular up"), recorder.calls)
    }

    /**
     * A down event follows an up event for the same network in the framework.
     * Attaching the listener *after* the network came up also checks that the
     * transport is remembered independently of who was listening at the time.
     */
    @Test
    fun onLost_reports_the_cellular_transport() {
        binder.onAvailable(cellular)
        val recorder = Recorder()
        binder.events = recorder

        binder.onLost(cellular)

        assertEquals(listOf("cellular down"), recorder.calls)
    }

    @Test
    fun onAvailable_reports_the_wifi_transport() {
        val recorder = Recorder()
        binder.events = recorder

        binder.onAvailable(wifi)

        assertEquals(listOf("wifi up"), recorder.calls)
    }

    @Test
    fun onLost_reports_the_wifi_transport() {
        binder.onAvailable(wifi)
        val recorder = Recorder()
        binder.events = recorder

        binder.onLost(wifi)

        assertEquals(listOf("wifi down"), recorder.calls)
    }

    /**
     * The framework delivers `onLost` only *after* the network is gone, so a
     * re-query of `getNetworkCapabilities()` returns `null` at that point. The
     * binder must remember the transport from `onAvailable` and report the down
     * from that record — otherwise the bridge never learns its uplink died.
     */
    @Test
    fun onLost_survives_the_capabilities_being_gone() {
        binder.onAvailable(cellular)
        binder.onAvailable(wifi)
        // As the framework does when a network is torn down.
        shadowOf(cm).setNetworkCapabilities(cellular, null)
        shadowOf(cm).setNetworkCapabilities(wifi, null)

        val recorder = Recorder()
        binder.events = recorder

        binder.onLost(cellular)
        binder.onLost(wifi)

        assertEquals(listOf("cellular down", "wifi down"), recorder.calls)
    }

    /**
     * Events are keyed on the *transport*, not on network identity: Android
     * hands out a fresh [Network] object after a re-connect.
     */
    @Test
    fun a_reconnected_wifi_network_still_reports() {
        val reconnected = addNetwork(NET_ID_WIFI + 10, NetworkCapabilities.TRANSPORT_WIFI)
        val recorder = Recorder()
        binder.events = recorder

        binder.onAvailable(reconnected)

        assertEquals(listOf("wifi up"), recorder.calls)
    }

    @Test
    fun transport_events_ignore_unrelated_networks() {
        val vpn = addNetwork(NET_ID_WIFI + 20, NetworkCapabilities.TRANSPORT_VPN)
        val recorder = Recorder()
        binder.events = recorder

        binder.onAvailable(vpn)
        binder.onLost(vpn)

        assertTrue("an unrelated transport must not fire", recorder.calls.isEmpty())
    }

    @Test
    fun transport_events_ignore_a_network_with_no_capabilities() {
        val unknown = addNetwork(NET_ID_WIFI + 30, transport = null)
        val recorder = Recorder()
        binder.events = recorder

        binder.onAvailable(unknown)
        binder.onLost(unknown)

        assertTrue("a network with unknown capabilities must not fire", recorder.calls.isEmpty())
    }

    @Test
    fun transport_events_are_silent_when_no_listener_is_attached() {
        // Must not throw.
        binder.onAvailable(cellular)
        binder.onLost(wifi)
    }

    @Test
    fun start_registers_once_and_is_idempotent() {
        val shadow = shadowOf(cm)
        assertEquals("nothing is registered before start", 0, shadow.networkCallbacks.size)

        binder.start()
        val afterFirst = shadow.networkCallbacks.size
        assertTrue("start registers a callback", afterFirst > 0)

        binder.start()
        assertEquals("a second start must not leak another callback", afterFirst, shadow.networkCallbacks.size)
    }

    @Test
    fun stop_unregisters_everything_and_is_idempotent() {
        binder.start()
        binder.stop()

        assertTrue("stop unregisters every callback", shadowOf(cm).networkCallbacks.isEmpty())

        binder.stop() // Must not throw.
        assertTrue(shadowOf(cm).networkCallbacks.isEmpty())
    }

    @Test
    fun bindWifiSocket_binds_the_socket_to_the_wifi_network() {
        var handedOver: DatagramSocket? = null
        val binder = NetworkBinder(cm) { socket -> handedOver = socket; STUB_FD }

        val fd = binder.bindWifiSocket(wifi)

        assertEquals(STUB_FD, fd)
        val socket = requireNotNull(handedOver)
        assertTrue("the socket must be bound to Wi-Fi", shadowOf(wifi).isSocketBound(socket))
    }

    /**
     * API 29+: `fromDatagramSocket` hands back a *dup*, so Rust's descriptor is
     * independent of the socket. Closing the socket is then safe and avoids
     * leaking one descriptor per handover.
     */
    @Test
    fun on_api_29_and_later_the_socket_is_closed_after_handover() {
        assertTrue(Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q)

        var handedOver: DatagramSocket? = null
        val binder = NetworkBinder(cm) { socket -> handedOver = socket; STUB_FD }

        binder.bindWifiSocket(wifi)

        assertTrue("the dup makes closing the socket safe", requireNotNull(handedOver).isClosed)
    }

    /**
     * Pre-API-29 `fromDatagramSocket` does *not* dup: the ParcelFileDescriptor
     * and the DatagramSocket share one descriptor. The socket must therefore be
     * retained and never closed, or its finalizer could close the descriptor
     * Rust now owns.
     *
     * The handover lambda keeps only a *weak* reference to the socket, so the
     * binder's retention map is the only thing that can keep it alive. The
     * retention is asserted directly on that map — a GC probe alone would pass by
     * luck if the object had not yet been collected, whereas the map check fails
     * the instant `retained[fd] = socket` is removed.
     */
    @Test
    @Config(sdk = [28])
    fun before_api_29_the_socket_is_retained_so_its_finalizer_cannot_close_the_fd() {
        assertTrue(Build.VERSION.SDK_INT < Build.VERSION_CODES.Q)

        val socketRef = arrayOfNulls<WeakReference<DatagramSocket>>(1)
        val binder = NetworkBinder(cm) { socket -> socketRef[0] = WeakReference(socket); STUB_FD }

        binder.bindWifiSocket(wifi)

        System.gc()
        System.runFinalization()

        val socket = requireNotNull(socketRef[0]).get()
        assertNotNull("the retained socket must stay reachable", socket)
        assertFalse("the shared descriptor must stay open", socket!!.isClosed)

        val retained = retainedSockets(binder)
        assertTrue("the handed-over fd must be retained", STUB_FD in retained)
        assertSame("retention must hold the exact socket handed over", socket, retained[STUB_FD])
    }

    /** Reads the binder's private retention map; the mechanism under test. */
    @Suppress("UNCHECKED_CAST")
    private fun retainedSockets(binder: NetworkBinder): Map<Int, DatagramSocket> {
        val field = NetworkBinder::class.java.getDeclaredField("retained")
        field.isAccessible = true
        return field.get(binder) as Map<Int, DatagramSocket>
    }

    // `ShadowConnectivityManager` keys networks on the legacy `NetworkInfo` type
    // and has no non-deprecated way to register a bare network; the deprecated
    // `NetworkInfo`/`ShadowNetworkInfo` API is used deliberately here only.
    @Suppress("DEPRECATION")
    private fun addNetwork(id: Int, transport: Int?): Network {
        val network = ShadowNetwork.newInstance(id)
        val info = ShadowNetworkInfo.newInstance(
            NetworkInfo.DetailedState.CONNECTED,
            if (transport == NetworkCapabilities.TRANSPORT_CELLULAR) {
                ConnectivityManager.TYPE_MOBILE
            } else {
                ConnectivityManager.TYPE_WIFI
            },
            0,
            true,
            true,
        )
        val shadow = shadowOf(cm)
        shadow.addNetwork(network, info)
        if (transport != null) {
            val capabilities = ShadowNetworkCapabilities.newInstance()
            shadowOf(capabilities).addTransportType(transport)
            if (transport == NetworkCapabilities.TRANSPORT_CELLULAR) {
                shadowOf(capabilities).addCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
            }
            shadow.setNetworkCapabilities(network, capabilities)
        }
        return network
    }

    private class Recorder : NetworkEvents {
        val calls = mutableListOf<String>()

        override fun onCellularChanged(up: Boolean) {
            calls += if (up) "cellular up" else "cellular down"
        }

        override fun onWifiChanged(up: Boolean) {
            calls += if (up) "wifi up" else "wifi down"
        }
    }

    private companion object {
        const val NET_ID_WIFI = 1
        const val NET_ID_CELLULAR = 2
        const val STUB_FD = 4242
    }
}
