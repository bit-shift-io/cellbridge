import 'package:flutter/material.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';

import 'src/bridge.dart' as bridge;
import 'src/config_screen.dart';
import 'src/dashboard.dart';
import 'src/demo_bridge.dart';
import 'src/settings_store.dart';
import 'src/android_bridge.dart';
import 'src/diagnostics_screen.dart';
import 'src/qr_scan_screen.dart';
import 'src/router_setup_screen.dart';

/// Runs the dashboard against the simulated backend on desktop:
///
///     flutter run -d linux --dart-define=CELLBRIDGE_DEMO=true
const bool _demoMode = bool.fromEnvironment('CELLBRIDGE_DEMO');

/// Placeholder credentials for the demo; [DemoBridgeApi] ignores them.
const _demoConfig = bridge.BridgeConfig(
  privateKey: 'demo-private-key',
  address: '10.7.0.2/24',
  peerPublicKey: 'demo-peer-public-key',
  endpoint: '192.0.2.1:51820',
  mtu: 1420,
);

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();

  DemoBridgeApi? demoApi;
  SettingsStore? store;
  bridge.BridgeConfig? config;

  if (_demoMode) {
    demoApi = DemoBridgeApi();
    bridge.bridgeApi = demoApi;
    config = _demoConfig;
  } else {
    await bridge.initBridge();
    if (defaultTargetPlatform == TargetPlatform.android) {
      final api = AndroidBridgeApi();
      await api.initialize();
      bridge.bridgeApi = api;
    }
    store = SettingsStore(backend: createSettingsBackend());
    try {
      config = await store.load();
    } catch (error) {
      // A missing Android channel (Task 10) lands here. Surface it and start
      // with no configuration rather than pretending an unencrypted store
      // exists.
      debugPrint('Could not load saved settings (${error.runtimeType}).');
    }
  }

  runApp(CellBridgeApp(demoApi: demoApi, store: store, initialConfig: config));
}

class CellBridgeApp extends StatefulWidget {
  const CellBridgeApp({
    super.key,
    this.demoApi,
    this.store,
    this.initialConfig,
  });

  /// Non-null only in demo mode; supplies the fake backend's error trigger.
  final DemoBridgeApi? demoApi;

  /// The settings store behind the config screen. `null` in tests that only
  /// exercise the dashboard.
  final SettingsStore? store;

  /// The configuration loaded at startup, if any.
  final bridge.BridgeConfig? initialConfig;

  @override
  State<CellBridgeApp> createState() => _CellBridgeAppState();
}

class _CellBridgeAppState extends State<CellBridgeApp> {
  /// Pushes routes through the [MaterialApp]'s Navigator.
  ///
  /// This state builds the `MaterialApp`, so its own `context` sits *above*
  /// the Navigator: `Navigator.of(context)` here throws "does not include a
  /// Navigator". The key gives the app-level state a handle on the Navigator
  /// it created without relying on that context.
  final _navigatorKey = GlobalKey<NavigatorState>();

  bridge.BridgeConfig? _config;

  /// Guards the one-shot first-launch auto-open so a rebuild can never stack a
  /// second config screen.
  bool _autoOpened = false;

  @override
  void initState() {
    super.initState();
    _config =
        widget.initialConfig ?? (widget.demoApi != null ? _demoConfig : null);
    // On first launch with nothing saved, open the config screen so the
    // dashboard's "no configuration" path guides the user instead of
    // dead-ending.
    if (widget.store != null && _config == null) {
      WidgetsBinding.instance.addPostFrameCallback((_) => _autoOpenConfig());
    }
  }

  /// Opens the config screen on first launch, but never twice and never on top
  /// of a route the user already opened.
  Future<void> _autoOpenConfig() async {
    if (_autoOpened) {
      return;
    }
    _autoOpened = true;
    final navigator = _navigatorKey.currentState;
    // `canPop` is true once anything has been pushed on top of the dashboard,
    // so a user who navigated during startup is not fought.
    if (navigator == null || navigator.canPop()) {
      return;
    }
    await _openConfig();
  }

  Future<void> _openConfig() async {
    final store = widget.store;
    final navigator = _navigatorKey.currentState;
    if (store == null || navigator == null) {
      return;
    }
    await navigator.push<void>(
      MaterialPageRoute(
        builder: (_) => ConfigScreen(
          store: store,
          initialConfig: _config,
          setupRouter: defaultTargetPlatform == TargetPlatform.android
              ? (context, privateKey) => Navigator.of(context).push<bridge.BridgeConfig>(
                  MaterialPageRoute(builder: (_) => RouterSetupScreen(
                    store: store, existingPrivateKey: privateKey,
                    bridgeRunning: () => bridge.bridgeApi.status().state != bridge.BridgeState.idle,
                  )))
              : null,
          testConnection: defaultTargetPlatform == TargetPlatform.android
              ? (config) =>
                    const MethodChannel('dev.cellbridge/bridge')
                        .invokeMethod<void>('testConnection', {
                          'privateKey': config.privateKey,
                          'address': config.address,
                          'peerPublicKey': config.peerPublicKey,
                          'endpoint': config.endpoint,
                          'mtu': config.mtu,
                          'presharedKey': config.presharedKey,
                        })
              : null,
          scanSetupQr: defaultTargetPlatform == TargetPlatform.android
              ? (context) => Navigator.of(context).push<String>(
                  MaterialPageRoute(builder: (_) => const QrScanScreen()),
                )
              : null,
          bridgeRunning: () =>
              bridge.bridgeApi.status().state != bridge.BridgeState.idle,
          onSaved: (config) {
            if (mounted) {
              setState(() => _config = config);
            }
          },
        ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final demo = widget.demoApi;
    return MaterialApp(
      title: 'CellBridge',
      navigatorKey: _navigatorKey,
      theme: ThemeData(
        colorSchemeSeed: Colors.teal,
        brightness: Brightness.dark,
        useMaterial3: true,
      ),
      themeMode: ThemeMode.dark,
      home: Dashboard(
        config: () => _config,
        actions: [
          if (bridge.bridgeApi case final AndroidBridgeApi api)
            IconButton(
              tooltip: 'Diagnostics',
              icon: const Icon(Icons.monitor_heart_outlined),
              onPressed: () => _navigatorKey.currentState?.push<void>(
                MaterialPageRoute(builder: (_) => DiagnosticsScreen(api: api)),
              ),
            ),
          if (widget.store != null)
            IconButton(
              key: const Key('open-config'),
              tooltip: 'Configuration',
              icon: const Icon(Icons.settings_outlined),
              onPressed: _openConfig,
            ),
          if (demo != null)
            IconButton(
              key: const Key('simulate-error'),
              tooltip: 'Simulate an error',
              icon: const Icon(Icons.bug_report_outlined),
              onPressed: () => demo.triggerError(),
            ),
        ],
      ),
    );
  }
}
