package dev.cellbridge

import android.content.Context
import android.content.SharedPreferences
import io.flutter.plugin.common.MethodCall
import io.flutter.plugin.common.MethodChannel
import io.flutter.plugin.common.StandardMethodCodec
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

/**
 * The Android side of the `dev.cellbridge/settings` channel: a [ConfigStore]
 * behind a thin [ConfigStoreHandler].
 *
 * `EncryptedSharedPreferences` cannot run under Robolectric — it needs the real
 * Android Keystore, and `MasterKey.Builder(...).build()` fails here with
 * `KeyStoreException`/`NoSuchAlgorithmException` (verified; see the report). The
 * suite therefore splits the layers the way the production code does:
 *
 * - [ConfigStoreHandler] (validation, dispatch, error mapping, codec typing) is
 *   driven against an in-memory [FakeConfigStore].
 * - [SharedPrefsConfigStore] (the record <-> key/value mapping, including
 *   "replace, never append" and the preshared-key omission rule) is driven
 *   against a real Robolectric `SharedPreferences`. It is the exact mapping the
 *   encrypted store delegates to; only the *encryption wrapper* is unexercised.
 *
 * The encrypted wrapper itself — that the bytes on disk are unreadable without
 * the Keystore key — is a device matter and is not claimed by this suite.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class ConfigStoreTest {

    private lateinit var store: FakeConfigStore
    private lateinit var handler: ConfigStoreHandler

    @Before
    fun setUp() {
        store = FakeConfigStore()
        handler = ConfigStoreHandler(store)
    }

    // ------------------------------------------------------------------
    // Handler: dispatch and validation
    // ------------------------------------------------------------------

    @Test
    fun read_returns_null_when_nothing_is_stored() {
        val result = read()

        assertTrue("read must succeed", result.succeeded)
        assertNull("an empty store must read as null", result.value)
    }

    @Test
    fun write_then_read_round_trips_every_field_including_the_preshared_key() {
        val written = write(fullRecord(preshared = true))
        assertNull("a valid write must not error", written.errorCode)

        val read = read()
        assertEquals(
            mapOf(
                "privateKey" to PRIVATE_KEY,
                "address" to "10.7.0.2/24",
                "peerPublicKey" to PEER_PUBLIC_KEY,
                "presharedKey" to PRESHARED_KEY,
                "endpoint" to "192.0.2.1:51820",
                "mtu" to 1420,
            ),
            read.value,
        )
    }

    @Test
    fun a_map_without_a_preshared_key_round_trips_without_that_key() {
        write(fullRecord(preshared = false))

        @Suppress("UNCHECKED_CAST")
        val record = read().value as Map<String, Any?>

        assertFalse(
            "an unset preshared key must be absent, never an empty string",
            record.containsKey("presharedKey"),
        )
    }

    @Test
    fun a_blank_preshared_key_is_omitted_rather_than_stored_as_empty() {
        write(fullRecord(preshared = false) + ("presharedKey" to "   "))

        @Suppress("UNCHECKED_CAST")
        val record = read().value as Map<String, Any?>

        assertFalse("a blank preshared key means 'not set'", record.containsKey("presharedKey"))
    }

    @Test
    fun write_replaces_the_previous_record_rather_than_appending_to_it() {
        write(fullRecord(preshared = true))

        val replacement = mapOf(
            "privateKey" to PRIVATE_KEY,
            "address" to "10.9.0.2/24",
            "peerPublicKey" to PEER_PUBLIC_KEY,
            "endpoint" to "198.51.100.7:51820",
            "mtu" to 1280,
        )
        write(replacement)

        @Suppress("UNCHECKED_CAST")
        val record = read().value as Map<String, Any?>

        assertEquals(
            "the second write must fully replace the first",
            replacement,
            record,
        )
        assertFalse("the first record's preshared key must not survive", record.containsKey("presharedKey"))
        assertEquals("the second write must be the only stored record", 2, store.writes)
    }

    @Test
    fun a_map_missing_the_private_key_is_rejected_without_storing_anything() {
        val result = write(fullRecord().minus("privateKey"))

        assertNotNull("a malformed map must produce an error", result.errorCode)
        assertNull("a rejected write must not touch the store", store.record)
    }

    @Test
    fun a_map_missing_the_endpoint_is_rejected_without_storing_anything() {
        val result = write(fullRecord().minus("endpoint"))

        assertNotNull(result.errorCode)
        assertNull(store.record)
    }

    @Test
    fun an_mtu_that_is_not_an_integer_is_rejected() {
        val result = write(fullRecord() + ("mtu" to "1420"))

        assertNotNull("a string MTU must be rejected", result.errorCode)
        assertNull(store.record)
    }

    @Test
    fun a_preshared_key_that_is_not_a_string_is_rejected() {
        val result = write(fullRecord() + ("presharedKey" to 42))

        assertNotNull(result.errorCode)
        assertNull(store.record)
    }

    @Test
    fun a_non_map_argument_is_rejected() {
        val result = write("not a map")

        assertNotNull(result.errorCode)
        assertNull(store.record)
    }

    @Test
    fun a_rejection_message_does_not_echo_key_material() {
        val result = write(fullRecord().minus("address"))

        assertNotNull(result.errorCode)
        assertNotNull(result.errorMessage)
        assertFalse(
            "the error must not quote the private key",
            result.errorMessage!!.contains(PRIVATE_KEY),
        )
        assertFalse(
            "the error must not quote the preshared key",
            result.errorMessage!!.contains(PRESHARED_KEY),
        )
    }

    @Test
    fun unknown_methods_are_reported_as_not_implemented() {
        val result = RecordingResult()

        handler.onMethodCall(MethodCall("delete", null), result)

        assertTrue(result.notImplemented)
    }

    @Test
    fun mtu_is_an_integer_in_the_read_result() {
        write(fullRecord())

        @Suppress("UNCHECKED_CAST")
        val record = read().value as Map<String, Any?>

        assertTrue("mtu must be an Int, not a String or Long", record["mtu"] is Int)
        assertEquals(1420, record["mtu"])
    }

    /**
     * Crosses the real [StandardMethodCodec] in both directions: the argument
     * map as the platform decodes a Dart `write`, and the reply as it encodes a
     * `read` back to Dart. A non-integer `mtu` would fail this.
     */
    @Test
    fun mtu_survives_the_channel_codec_as_an_integer() {
        val codec = StandardMethodCodec.INSTANCE
        val encodedCall = codec.encodeMethodCall(MethodCall("write", fullRecord(preshared = false)))
        encodedCall.flip()
        val decodedArguments = codec.decodeMethodCall(encodedCall).arguments

        val writeResult = RecordingResult()
        handler.onMethodCall(MethodCall("write", decodedArguments), writeResult)
        assertNull(writeResult.errorCode)

        val readResult = read()
        val encodedReply = codec.encodeSuccessEnvelope(readResult.value)
        encodedReply.flip()
        val decoded = codec.decodeEnvelope(encodedReply)

        @Suppress("UNCHECKED_CAST")
        val record = decoded as Map<String, Any?>
        assertEquals(1420, record["mtu"])
        assertTrue("mtu must cross the codec as an Int", record["mtu"] is Int)
        assertFalse("an absent preshared key must not appear", record.containsKey("presharedKey"))
    }

    // ------------------------------------------------------------------
    // SharedPrefsConfigStore: the mapping the encrypted store delegates to
    // ------------------------------------------------------------------

    @Test
    fun sharedPrefs_store_reads_null_when_empty() {
        assertNull(prefsStore("empty").read())
    }

    @Test
    fun sharedPrefs_store_round_trips_a_full_record() {
        val prefs = plainPrefs("round-trip")
        val store = SharedPrefsConfigStore(prefs)

        store.write(fullRecord(preshared = true))

        assertEquals(
            mapOf(
                "privateKey" to PRIVATE_KEY,
                "address" to "10.7.0.2/24",
                "peerPublicKey" to PEER_PUBLIC_KEY,
                "presharedKey" to PRESHARED_KEY,
                "endpoint" to "192.0.2.1:51820",
                "mtu" to 1420,
            ),
            store.read(),
        )
    }

    @Test
    fun sharedPrefs_store_omits_an_unset_preshared_key() {
        val store = prefsStore("omit-psk")

        store.write(fullRecord(preshared = false))

        val record = store.read()!!
        assertFalse("the key must be absent, not an empty string", record.containsKey("presharedKey"))
    }

    /**
     * The exact regression the "replace, not append" rule guards: a second write
     * without a preshared key must remove the first write's key, and change every
     * other field in place.
     */
    @Test
    fun sharedPrefs_store_write_replaces_the_record() {
        val store = prefsStore("replace")
        store.write(fullRecord(preshared = true))

        val replacement = mapOf(
            "privateKey" to PRIVATE_KEY,
            "address" to "10.9.0.2/24",
            "peerPublicKey" to PEER_PUBLIC_KEY,
            "endpoint" to "198.51.100.7:51820",
            "mtu" to 1280,
        )
        store.write(replacement)

        assertEquals(replacement, store.read())
    }

    @Test
    fun sharedPrefs_store_keeps_mtu_an_integer_on_read() {
        val store = prefsStore("mtu-int")
        store.write(fullRecord())

        assertTrue("mtu must read back as an Int", store.read()!!["mtu"] is Int)
    }

    // ------------------------------------------------------------------
    // Fixtures
    // ------------------------------------------------------------------

    private fun read(): RecordingResult {
        val result = RecordingResult()
        handler.onMethodCall(MethodCall("read", null), result)
        return result
    }

    private fun write(arguments: Any?): RecordingResult {
        val result = RecordingResult()
        handler.onMethodCall(MethodCall("write", arguments), result)
        return result
    }

    private fun fullRecord(preshared: Boolean = true): Map<String, Any?> = buildMap {
        put("privateKey", PRIVATE_KEY)
        put("address", "10.7.0.2/24")
        put("peerPublicKey", PEER_PUBLIC_KEY)
        if (preshared) {
            put("presharedKey", PRESHARED_KEY)
        }
        put("endpoint", "192.0.2.1:51820")
        put("mtu", 1420)
    }

    private fun plainPrefs(name: String): SharedPreferences =
        RuntimeEnvironment.getApplication().getSharedPreferences(name, Context.MODE_PRIVATE)

    private fun prefsStore(name: String): SharedPrefsConfigStore {
        val prefs = plainPrefs(name)
        prefs.edit().clear().commit()
        return SharedPrefsConfigStore(prefs)
    }

    /** In-memory [ConfigStore] so the handler never touches the Keystore. */
    private class FakeConfigStore : ConfigStore {
        var record: Map<String, Any?>? = null
        var writes = 0

        override fun read(): Map<String, Any?>? = record

        override fun write(value: Map<String, Any?>) {
            record = value
            writes++
        }
    }

    /** Records what the channel reply would be, without a real channel. */
    private class RecordingResult : MethodChannel.Result {
        var value: Any? = null
        var succeeded = false
        var errorCode: String? = null
        var errorMessage: String? = null
        var notImplemented = false

        override fun success(result: Any?) {
            succeeded = true
            value = result
        }

        override fun error(errorCode: String, errorMessage: String?, errorDetails: Any?) {
            this.errorCode = errorCode
            this.errorMessage = errorMessage
        }

        override fun notImplemented() {
            notImplemented = true
        }
    }

    private companion object {
        val PRIVATE_KEY = "A".repeat(43) + "="
        val PEER_PUBLIC_KEY = "B".repeat(43) + "="
        val PRESHARED_KEY = "C".repeat(43) + "="
    }
}
