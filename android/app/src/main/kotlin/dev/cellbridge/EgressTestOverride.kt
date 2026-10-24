package dev.cellbridge

import android.content.ContentResolver
import android.provider.Settings

/** adb-only handshake testing. Release builds never consult the setting. */
object EgressTestOverride {
    fun enabled(debugBuild: Boolean, resolver: ContentResolver): Boolean =
        debugBuild && Settings.Global.getInt(resolver, "cellbridge_egress_test", 0) == 1
}
