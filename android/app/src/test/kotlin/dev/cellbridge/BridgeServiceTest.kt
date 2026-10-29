package dev.cellbridge

import android.content.Context
import android.content.Intent
import android.app.Notification
import android.app.NotificationManager
import org.json.JSONObject
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

    @Test fun notification_title_is_the_status() {
        // Title carries the status; the old "CellBridge" title and the
        // content-text subtitle are gone.
        assertEquals("Connected", notificationTitle("connected"))
        assertEquals("Disconnected", notificationTitle("connecting"))
        assertEquals("Disconnected", notificationTitle("idle"))
        assertEquals("Disconnected", notificationTitle("error"))
    }

    @Test fun notification_subtitle_is_the_speed_pair_only_when_connected() {
        val connected = JSONObject().put("state", "connected")
            .put("rxBytesPerSec", "5000000").put("txBytesPerSec", "3750000")
        assertEquals("40-30", notificationSubtitle(connected))
        for (state in listOf("connecting", "idle", "error")) {
            assertNull(notificationSubtitle(JSONObject().put("state", state)))
        }
        assertNull(notificationSubtitle(null))
        assertEquals("0-0", notificationSubtitle(JSONObject().put("state", "connected")))
    }

    @Test fun format_mbps_uses_the_shared_rule() {
        assertEquals("40", formatMbps("5000000"))
        assertEquals("10", formatMbps("1250000"))
        assertEquals("3.4", formatMbps("425000"))
        assertEquals("0.4", formatMbps("50000"))
        assertEquals("0", formatMbps("0"))
        assertEquals("0", formatMbps("not-a-number"))
        assertEquals("0.2", formatMbps("18750"))
        assertEquals("1", formatMbps("118750"))
        assertEquals("10", formatMbps("1243750"))
        assertEquals("147573952589676", formatMbps("18446744073709551615"))
    }

    @Test fun failure_clears_the_previously_connected_notification_subtitle() {
        val service = Robolectric.buildService(BridgeService::class.java).create()
        try {
            val instance = service.get()
            val launch = Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER)
                .setPackage(instance.packageName)
            shadowOf(instance.packageManager).addResolveInfoForIntent(launch,
                android.content.pm.ResolveInfo().apply {
                    activityInfo = android.content.pm.ActivityInfo().apply {
                        packageName = instance.packageName
                        name = "dev.cellbridge.cellbridge.MainActivity"
                    }
                })
            val manager = instance.getSystemService(NotificationManager::class.java)
            manager.notify(51820, Notification.Builder(instance, "cellbridge")
                .setContentTitle("Connected").setContentText("40-30").build())
            BridgeService::class.java.getDeclaredMethod("fail", String::class.java).apply {
                isAccessible = true
            }.invoke(instance, "Test failure")
            val notification = shadowOf(manager).allNotifications.single()
            assertEquals("Disconnected", notification.extras.getString(Notification.EXTRA_TITLE))
            assertNull(notification.extras.getString(Notification.EXTRA_TEXT))
        } finally {
            service.destroy()
        }
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
