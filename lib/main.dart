import 'package:flutter/material.dart';

import 'src/bridge.dart' as bridge;

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await bridge.initBridge();
  runApp(CellBridgeApp(ping: bridge.ping()));
}

class CellBridgeApp extends StatelessWidget {
  const CellBridgeApp({super.key, required this.ping});

  /// Result of the Rust round-trip invoked at startup.
  final Future<String> ping;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'CellBridge',
      home: HomePage(ping: ping),
    );
  }
}

class HomePage extends StatelessWidget {
  const HomePage({super.key, required this.ping});

  /// Result of the Rust round-trip invoked at startup.
  final Future<String> ping;

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('CellBridge')),
      body: Center(
        child: FutureBuilder<String>(
          future: ping,
          builder: (context, snapshot) {
            if (snapshot.hasError) {
              return Text('Rust bridge error: ${snapshot.error}');
            }
            if (snapshot.hasData) {
              return Text(snapshot.data!);
            }
            return const CircularProgressIndicator();
          },
        ),
      ),
    );
  }
}
