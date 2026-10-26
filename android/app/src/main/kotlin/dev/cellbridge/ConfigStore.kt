package dev.cellbridge

import android.content.Context
import android.content.SharedPreferences
import android.util.Log
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey
import io.flutter.plugin.common.MethodCall
import io.flutter.plugin.common.MethodChannel
import java.io.IOException
import java.security.GeneralSecurityException

/**
 * The channel the Dart side (`lib/src/settings_store.dart`) speaks to.
 *
 * Kept here so `MainActivity` registers the exact name the Dart backend uses.
 */
const val SETTINGS_CHANNEL = "dev.cellbridge/settings"

/**
 * Persists the bridge configuration as a single record.
 *
 * [read] returns the whole record, or `null` when nothing is stored. [write]
 * *replaces* the record. Implementations are deliberately small so the channel
 * handler can be tested against an in-memory fake: production uses
 * [EncryptedConfigStore], which needs the Android Keystore.
 */
interface ConfigStore {
    /** The stored record, or `null` when the store is empty. */
    fun read(): Map<String, Any?>?

    /** Replaces the stored record with [value]. */
    fun write(value: Map<String, Any?>)
}

/**
 * A [ConfigStore] over a [SharedPreferences] instance.
 *
 * This is the record <-> key/value mapping, separated from *how* the
 * preferences are created. Production hands it the encrypted store
 * ([EncryptedConfigStore]); tests hand it a plain in-memory instance so the
 * mapping can be exercised without the Keystore.
 *
 * It stores each field under its own key. The preshared key is written only when
 * non-blank and removed otherwise, so "unset" round-trips as an *absent* key
 * rather than an empty string. [write] replaces every known key, so a second
 * write never leaves a previous write's field behind.
 */
class SharedPrefsConfigStore(private val prefs: SharedPreferences) : ConfigStore {

    override fun read(): Map<String, Any?>? {
        if (!prefs.contains(KEY_PRIVATE_KEY)) {
            return null
        }
        val record = mutableMapOf<String, Any?>()
        record["privateKey"] = prefs.getString(KEY_PRIVATE_KEY, null)
        record["address"] = prefs.getString(KEY_ADDRESS, null)
        record["peerPublicKey"] = prefs.getString(KEY_PEER_PUBLIC_KEY, null)
        prefs.getString(KEY_PRESHARED_KEY, null)
            ?.takeIf { it.isNotEmpty() }
            ?.let { record["presharedKey"] = it }
        record["endpoint"] = prefs.getString(KEY_ENDPOINT, null)
        record["mtu"] = prefs.getInt(KEY_MTU, 0)
        return record
    }

    override fun write(value: Map<String, Any?>) {
        val editor = prefs.edit()
        editor.putString(KEY_PRIVATE_KEY, value["privateKey"] as? String)
        editor.putString(KEY_ADDRESS, value["address"] as? String)
        editor.putString(KEY_PEER_PUBLIC_KEY, value["peerPublicKey"] as? String)
        editor.putString(KEY_ENDPOINT, value["endpoint"] as? String)

        val presharedKey = value["presharedKey"] as? String
        if (presharedKey.isNullOrEmpty()) {
            editor.remove(KEY_PRESHARED_KEY)
        } else {
            editor.putString(KEY_PRESHARED_KEY, presharedKey)
        }

        editor.putInt(KEY_MTU, (value["mtu"] as? Number)?.toInt() ?: 0)
        editor.commit()
    }

    companion object {
        const val KEY_PRIVATE_KEY = "privateKey"
        const val KEY_ADDRESS = "address"
        const val KEY_PEER_PUBLIC_KEY = "peerPublicKey"
        const val KEY_PRESHARED_KEY = "presharedKey"
        const val KEY_ENDPOINT = "endpoint"
        const val KEY_MTU = "mtu"
    }
}

/**
 * The production [ConfigStore]: a [SharedPrefsConfigStore] over
 * `EncryptedSharedPreferences`, so the WireGuard private key is not readable at
 * rest.
 *
 * There is deliberately **no plaintext fallback**. If the encrypted store cannot
 * be opened (no Keystore, corrupt master key), construction throws rather than
 * degrading to an unencrypted file: a bridge that silently stored its private
 * key in the clear would be worse than one that refuses to start.
 *
 * The Keystore means this cannot be constructed under Robolectric; only the
 * mapping it delegates to is unit-tested (see `ConfigStoreTest`).
 */
class EncryptedConfigStore(context: Context) : ConfigStore {

    private val delegate = SharedPrefsConfigStore(openEncryptedPreferences(context))

    override fun read(): Map<String, Any?>? = delegate.read()

    override fun write(value: Map<String, Any?>) = delegate.write(value)
}

/** The stable file backing the encrypted store. */
private const val ENCRYPTED_PREFS_FILE = "dev.cellbridge.settings"

/**
 * Open the encrypted preferences, or fail loudly.
 *
 * No `catch` falls back to an unencrypted store; the only handling is to wrap
 * the platform exception in one that names the failure. The wrapped message
 * carries no configuration values.
 */
internal fun openEncryptedPreferences(context: Context): SharedPreferences {
    try {
        @Suppress("DEPRECATION") // EncryptedSharedPreferences is the ruled-in store.
        val masterKey =
            MasterKey.Builder(context)
                .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
                .build()

        @Suppress("DEPRECATION")
        return EncryptedSharedPreferences.create(
            context,
            ENCRYPTED_PREFS_FILE,
            masterKey,
            EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
            EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
        )
    } catch (e: GeneralSecurityException) {
        throw IllegalStateException("could not open the encrypted settings store", e)
    } catch (e: IOException) {
        throw IllegalStateException("could not open the encrypted settings store", e)
    }
}

/**
 * Dispatches `read`/`write` from Dart onto a [ConfigStore].
 *
 * The handler validates the incoming map before it reaches the store, so a
 * malformed write is a channel error and never a half-written record. Error
 * messages describe the missing/wrong field, never its value, so no key material
 * can escape through an error. Logging is limited to the method name and
 * success/failure — the map is never logged, at any level.
 */
class ConfigStoreHandler(private val store: ConfigStore) : MethodChannel.MethodCallHandler {

    override fun onMethodCall(call: MethodCall, result: MethodChannel.Result) {
        when (call.method) {
            "read" ->
                try {
                    result.success(store.read())
                    Log.d(TAG, "read ok")
                } catch (e: RuntimeException) {
                    fail(result, "read", e)
                }

            "write" ->
                try {
                    store.write(validated(call.arguments))
                    result.success(null)
                    Log.d(TAG, "write ok")
                } catch (e: InvalidConfigException) {
                    Log.w(TAG, "write rejected: invalid configuration")
                    result.error(ERROR_INVALID_CONFIG, e.message, null)
                } catch (e: RuntimeException) {
                    fail(result, "write", e)
                }

            else -> result.notImplemented()
        }
    }

    /** Reports a store failure without leaking anything but the method name. */
    private fun fail(result: MethodChannel.Result, method: String, e: RuntimeException) {
        Log.w(TAG, "$method failed: ${e.javaClass.simpleName}")
        result.error(ERROR_STORE, "could not access the configuration store", null)
    }

    /**
     * Validate and normalise a `write` argument.
     *
     * Required fields must be non-blank strings; `mtu` must be an integer; the
     * preshared key is optional and blank means "unset". The returned map omits
     * the preshared key when unset, so it round-trips as absent.
     *
     * @throws InvalidConfigException with a value-free message.
     */
    private fun validated(arguments: Any?): Map<String, Any?> {
        if (arguments !is Map<*, *>) {
            throw InvalidConfigException("write expects a map")
        }
        fun requiredString(key: String): String {
            val value = arguments[key]
            if (value !is String || value.isBlank()) {
                throw InvalidConfigException("missing or empty \"$key\"")
            }
            return value
        }

        val mtu = arguments["mtu"]
        val mtuInt =
            when (mtu) {
                is Int -> mtu
                is Long -> if (mtu in Int.MIN_VALUE..Int.MAX_VALUE) mtu.toInt() else
                    throw InvalidConfigException("\"mtu\" is out of range")
                else -> throw InvalidConfigException("\"mtu\" is not an integer")
            }

        val record = mutableMapOf<String, Any?>()
        record["privateKey"] = requiredString("privateKey")
        record["address"] = requiredString("address")
        record["peerPublicKey"] = requiredString("peerPublicKey")
        val presharedKey = arguments["presharedKey"]
        if (presharedKey != null && presharedKey !is String) {
            throw InvalidConfigException("\"presharedKey\" is not a string")
        }
        (presharedKey as? String)
            ?.takeIf { it.isNotBlank() }
            ?.let { record["presharedKey"] = it }
        record["endpoint"] = requiredString("endpoint")
        record["mtu"] = mtuInt
        return record
    }

    private companion object {
        const val TAG = "ConfigStore"
        const val ERROR_INVALID_CONFIG = "invalid_config"
        const val ERROR_STORE = "store_error"
    }
}

/** A malformed `write` argument. The message never contains a field value. */
class InvalidConfigException(message: String) : Exception(message)
