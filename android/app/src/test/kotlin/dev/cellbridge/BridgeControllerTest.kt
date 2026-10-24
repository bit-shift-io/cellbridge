package dev.cellbridge

import android.net.Network
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.shadows.ShadowNetwork

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class BridgeControllerTest {
    @Test fun connection_test_reservation_excludes_bridge_start_in_both_directions() {
        val native = Native()
        val controller = BridgeController(Networks(), native) { it() }
        controller.start(emptyMap())
        assertFalse(BridgeController.tryBeginConnectionTest())
        controller.stop()
        assertTrue(BridgeController.tryBeginConnectionTest())
        try {
            assertFalse(BridgeController.tryBeginConnectionTest())
            controller.start(emptyMap())
            assertEquals(1, native.starts)
            assertTrue(controller.snapshot().getString("error").contains("test"))
        } finally { BridgeController.endConnectionTest() }
        controller.start(emptyMap())
        assertEquals(2, native.starts)
        controller.stop()
    }
    @Test fun test_connection_never_claims_cellular_verification_or_allows_cellular_control() {
        val networks = Networks(); val native = Native()
        val wifi = ShadowNetwork.newInstance(2)
        networks.selection = NetworkSelection(wifi, wifi, testOverride = true)
        val controller = BridgeController(networks, native) { it() }
        controller.start(emptyMap())
        assertEquals(1, native.starts)
        val binding = controller.snapshot().getJSONObject("binding")
        assertTrue(binding.getBoolean("egressTestOverride"))
        assertFalse(binding.getBoolean("cellularBindVerified"))
        try {
            controller.cellularControl()
            fail("Test connection must not claim cellular control")
        } catch (_: IllegalStateException) {}
        controller.stop()
    }
    @Test fun encrypted_configuration_failure_remains_visible_across_service_ticks() {
        val controller = BridgeController(Networks(), Native()) { it() }
        val broken = object : ConfigStore {
            override fun read(): Map<String, Any?>? = throw IllegalStateException("secret store details")
            override fun write(value: Map<String, Any?>) {}
        }
        controller.startFromStore(broken)
        repeat(3) { controller.refresh() }
        val snapshot = controller.snapshot()
        assertEquals("error", snapshot.getString("state"))
        assertTrue(snapshot.getString("error").contains("encrypted"))
        assertFalse(snapshot.toString().contains("secret store details"))
        controller.stop()
        assertEquals("idle", controller.snapshot().getString("state"))
    }
    @Test fun obsolete_controller_cleanup_cannot_unbind_replacement_session() {
        val oldNetworks = Networks(); val newNetworks = Networks(); val native = Native()
        val old = BridgeController(oldNetworks, native) { it() }
        val replacement = BridgeController(newNetworks, native) { it() }
        old.start(emptyMap())
        replacement.start(emptyMap())
        val unbindsBeforeObsoleteCleanup = oldNetworks.unbound
        val stopsBeforeObsoleteCleanup = native.stops
        old.stop(); old.refresh()
        assertEquals(unbindsBeforeObsoleteCleanup, oldNetworks.unbound)
        assertEquals(stopsBeforeObsoleteCleanup, native.stops)
        assertTrue(replacement.snapshot().getJSONObject("binding").getBoolean("cellularBindVerified"))
        replacement.stop()
    }
    private class Networks : BridgeNetworks {
        var selection: NetworkSelection? = NetworkSelection(ShadowNetwork.newInstance(1), ShadowNetwork.newInstance(2))
        var bindOk = true
        var unbound = 0
        var stopped = 0
        override fun start(changed: () -> Unit) {}
        override fun current() = selection
        override fun bindCellular(network: Network) = bindOk
        override fun wifiSocket(network: Network) = 77
        override fun unbind() { unbound++ }
        override fun stop() { stopped++ }
        override fun cellularControl(network: Network) = "203.0.113.1"
    }
    private class Native : BridgeFacade {
        var starts = 0
        var stops = 0
        var startupError: String? = null
        override fun start(configJson: String, fd: Int) {
            assertEquals(77, fd)
            startupError?.let { throw IllegalStateException(it) }
            starts++
        }
        override fun stop() { stops++ }
        override fun snapshotJson() = "{\"state\":\"connected\",\"txBytes\":\"9007199254740993\",\"rxBytes\":\"0\",\"activeFlows\":\"1\",\"diagnostics\":[]}"
    }

    @Test fun startup_reason_survives_refresh_without_exposing_unknown_exception_text() {
        val networks = Networks(); val native = Native()
        val controller = BridgeController(networks, native) { it() }
        for (reason in listOf("Invalid phone private key", "Invalid peer endpoint")) {
            native.startupError = reason
            controller.start(emptyMap())
            repeat(3) { controller.refresh() }
            assertTrue(controller.snapshot().getString("error").contains(reason))
            controller.stop()
        }
        native.startupError = "secret configuration contents"
        controller.start(emptyMap())
        assertFalse(controller.snapshot().toString().contains("secret configuration contents"))
        controller.stop()
    }

    @Test fun repeated_start_is_single_session_and_explicit_stop_cancels_recovery() {
        val networks = Networks(); val native = Native()
        val controller = BridgeController(networks, native) { it() }
        controller.start(mapOf("privateKey" to "redacted")); controller.start(emptyMap())
        assertEquals(1, native.starts)
        assertEquals("connected", controller.snapshot().getString("state"))
        assertEquals("9007199254740993", controller.snapshot().getString("txBytes"))
        controller.stop(); controller.refresh()
        assertEquals(1, native.starts)
        assertEquals(1, native.stops)
        assertEquals("idle", controller.snapshot().getString("state"))
    }

    @Test fun missing_cellular_or_failed_binding_never_starts_rust() {
        val networks = Networks(); val native = Native()
        val controller = BridgeController(networks, native) { it() }
        networks.selection = null
        controller.start(emptyMap()); assertEquals(0, native.starts)
        networks.selection = NetworkSelection(ShadowNetwork.newInstance(1), ShadowNetwork.newInstance(2)); networks.bindOk = false
        controller.refresh(); assertEquals(0, native.starts)
        assertFalse(controller.snapshot().getJSONObject("binding").optBoolean("cellularBindVerified"))
        networks.bindOk = true; controller.refresh(); assertEquals(1, native.starts)
    }

    @Test fun network_replacement_stops_old_flows_and_stale_callbacks_do_not_restart_new_session() {
        val networks = Networks(); val native = Native()
        val controller = BridgeController(networks, native) { it() }
        controller.start(emptyMap())
        networks.selection = null; controller.refresh()
        assertEquals(1, native.stops)
        networks.selection = NetworkSelection(ShadowNetwork.newInstance(3), ShadowNetwork.newInstance(4)); controller.refresh()
        assertEquals(2, native.starts)
        controller.refresh() // stale old-network notification; current selection unchanged
        assertEquals(2, native.starts)
        assertEquals(1, native.stops)
        controller.stop()
    }
}
