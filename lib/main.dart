import 'package:flutter/material.dart';

import 'src/bridge.dart' as bridge;
import 'src/dashboard.dart';
import 'src/demo_bridge.dart';

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
  if (_demoMode) {
    demoApi = DemoBridgeApi();
    bridge.bridgeApi = demoApi;
  } else {
    await bridge.initBridge();
  }

  runApp(CellBridgeApp(demoApi: demoApi));
}

class CellBridgeApp extends StatelessWidget {
  const CellBridgeApp({super.key, this.demoApi});

  /// Non-null only in demo mode; supplies the fake backend's error trigger.
  final DemoBridgeApi? demoApi;

  @override
  Widget build(BuildContext context) {
    final demo = demoApi;
    return MaterialApp(
      title: 'CellBridge',
      theme: ThemeData(colorSchemeSeed: Colors.teal, useMaterial3: true),
      home: Dashboard(
        // Task 13 replaces this with the saved settings store.
        config: () => demo == null ? null : _demoConfig,
        actions: demo == null
            ? const []
            : [
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
