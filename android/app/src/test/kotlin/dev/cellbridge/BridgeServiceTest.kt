package dev.cellbridge

import android.content.Context
import android.content.Intent
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class BridgeServiceTest {
    @Test fun service_posts_foreground_notification_before_network_work() {
        val service = Robolectric.buildService(BridgeService::class.java).create()
        assertTrue(shadowOf(service.get()).isForegroundStopped.not())
        assertNotNull(shadowOf(service.get()).lastForegroundNotification)
        service.destroy()
    }

    @Test fun boot_starts_native_service_only_when_configuration_exists() {
        val context = RuntimeEnvironment.getApplication()
        val store = object : ConfigStore {
            var saved: Map<String, Any?>? = null
            override fun read() = saved
            override fun write(value: Map<String, Any?>) { saved = value }
        }
        val receiver = BootReceiver { store }
        receiver.onReceive(context, Intent(Intent.ACTION_BOOT_COMPLETED))
        assertNull(shadowOf(context).nextStartedService)
        store.write(mapOf("privateKey" to "redacted"))
        receiver.onReceive(context, Intent(Intent.ACTION_BOOT_COMPLETED))
        assertEquals(BridgeService::class.java.name, shadowOf(context).nextStartedService.component!!.className)
    }
}
