package dev.cellbridge

import android.content.Context
import java.net.InetAddress
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class RouterSetupStoreTest {
    @Test fun identity_and_host_pin_survive_new_store_without_changing_bridge_settings() {
        val prefs = RuntimeEnvironment.getApplication().getSharedPreferences("setup-test", Context.MODE_PRIVATE)
        prefs.edit().clear().putString("privateKey", "existing-bridge-key").commit()
        val store = RouterSetupStore(prefs)
        val key = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
        store.saveIdentity(key)
        store.savePin("192.168.8.1:22", "SHA256:test-fingerprint")
        val reloaded = RouterSetupStore(prefs)
        assertEquals(key, reloaded.identity())
        assertEquals("SHA256:test-fingerprint", reloaded.pin("192.168.8.1:22"))
        assertNull(reloaded.pin("192.168.8.2:22"))
        assertEquals("existing-bridge-key", prefs.getString("privateKey", null))
        assertFalse(prefs.contains("username"))
        assertFalse(prefs.contains("password"))
    }

    @Test fun invalid_identity_is_rejected_without_replacing_saved_key() {
        val prefs = RuntimeEnvironment.getApplication().getSharedPreferences("invalid-setup-test", Context.MODE_PRIVATE)
        val store = RouterSetupStore(prefs)
        val key = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
        store.saveIdentity(key)
        try { store.saveIdentity("invalid"); fail("must reject") } catch (_: IllegalArgumentException) {}
        assertEquals(key, store.identity())
    }

    @Test fun wifi_setup_lease_excludes_connection_tests_and_releases_on_close() {
        val lease = RouterSetupLease()
        assertTrue(lease.acquire())
        assertFalse(BridgeController.tryBeginConnectionTest())
        lease.close()
        lease.close()
        assertTrue(BridgeController.tryBeginConnectionTest())
        BridgeController.endConnectionTest()
    }

    @Test fun gateway_ignores_connected_routes_with_unspecified_next_hop() {
        val gateway = InetAddress.getByName("192.168.8.1")
        assertEquals("192.168.8.1", firstIpv4Gateway(listOf(null, gateway)))
        assertNull(firstIpv4Gateway(listOf(null)))
        assertNull(firstIpv4Gateway(listOf(InetAddress.getByName("fe80::1"))))
    }
}
