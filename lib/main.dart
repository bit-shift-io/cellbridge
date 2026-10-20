import 'package:flutter/material.dart';

import 'src/bridge.dart' as bridge;
import 'src/config_screen.dart';
import 'src/dashboard.dart';
import 'src/demo_bridge.dart';
import 'src/settings_store.dart';

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
  bridge.BridgeConfig? _config;

  @override
  void initState() {
    super.initState();
    _config =
        widget.initialConfig ?? (widget.demoApi != null ? _demoConfig : null);
    // On first launch with nothing saved, open the config screen so the
    // dashboard's "no configuration" path guides the user instead of
    // dead-ending.
    if (widget.store != null && _config == null) {
      WidgetsBinding.instance.addPostFrameCallback((_) => _openConfig());
    }
  }

  Future<void> _openConfig() async {
    final store = widget.store;
    if (store == null) {
      return;
    }
    await Navigator.of(context).push<void>(
      MaterialPageRoute(
        builder: (_) => ConfigScreen(
          store: store,
          initialConfig: _config,
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
      theme: ThemeData(colorSchemeSeed: Colors.teal, useMaterial3: true),
      home: Dashboard(
        config: () => _config,
        actions: [
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
