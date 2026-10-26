package dev.cellbridge

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class BridgeTileServiceTest {
    @Test fun idle_is_inactive() {
        val status = tileStatus("""{"state":"idle"}""")
        assertFalse(status.active)
        assertEquals("Idle", status.label)
    }

    @Test fun connected_is_active() {
        val status = tileStatus("""{"state":"connected"}""")
        assertTrue(status.active)
        assertEquals("Connected", status.label)
    }

    @Test fun connecting_is_active() {
        val status = tileStatus("""{"state":"connecting"}""")
        assertTrue(status.active)
        assertEquals("Connecting", status.label)
    }

    @Test fun error_is_active_so_it_can_be_stopped() {
        val status = tileStatus("""{"state":"error"}""")
        assertTrue(status.active)
        assertEquals("Error", status.label)
    }

    @Test fun malformed_snapshot_is_inactive() {
        val status = tileStatus("not json")
        assertFalse(status.active)
        assertEquals("Idle", status.label)
    }
}
