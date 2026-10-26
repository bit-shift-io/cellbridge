package dev.cellbridge.cellbridge

import dev.cellbridge.ConfigStoreHandler
import dev.cellbridge.EncryptedConfigStore
import dev.cellbridge.SETTINGS_CHANNEL
import dev.cellbridge.BRIDGE_CHANNEL
import dev.cellbridge.BridgeChannel
import dev.cellbridge.RouterSetupChannel
import dev.cellbridge.ROUTER_SETUP_CHANNEL
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel

class MainActivity : FlutterActivity() {
    private var routerSetup: RouterSetupChannel? = null

    /**
     * Register the settings channel on the engine that runs the app.
     *
     * `super` runs the generated plugin registrant first, so the engine is
     * fully configured before our handler is attached. The store is created
     * here (once) and fails loudly if the encrypted store cannot be opened;
     * there is no plaintext fallback.
     */
    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        MethodChannel(flutterEngine.dartExecutor.binaryMessenger, BRIDGE_CHANNEL)
            .setMethodCallHandler(BridgeChannel(this))

        val store = EncryptedConfigStore(applicationContext)
        MethodChannel(flutterEngine.dartExecutor.binaryMessenger, SETTINGS_CHANNEL)
            .setMethodCallHandler(ConfigStoreHandler(store))
        routerSetup = RouterSetupChannel(applicationContext)
        MethodChannel(flutterEngine.dartExecutor.binaryMessenger, ROUTER_SETUP_CHANNEL)
            .setMethodCallHandler(routerSetup)
    }

    override fun onDestroy() {
        routerSetup?.close()
        routerSetup = null
        super.onDestroy()
    }
}
