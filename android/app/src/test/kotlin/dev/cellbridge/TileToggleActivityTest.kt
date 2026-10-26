package dev.cellbridge

import android.content.Intent
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class TileToggleActivityTest {
    @Test fun starts_the_bridge_and_finishes() {
        val intent = Intent(RuntimeEnvironment.getApplication(), TileToggleActivity::class.java)
            .putExtra(TileToggleActivity.EXTRA_ACTION, BridgeService.START)
        val activity = Robolectric.buildActivity(TileToggleActivity::class.java, intent).setup().get()

        assertTrue(activity.isFinishing)
        assertEquals(
            BridgeService::class.java.name,
            shadowOf(activity).nextStartedService.component!!.className,
        )
    }
}
