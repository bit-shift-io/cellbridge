import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';

import 'android_bridge.dart';

class DiagnosticsScreen extends StatefulWidget {
  const DiagnosticsScreen({super.key, required this.api});
  final AndroidBridgeApi api;
  @override
  State<DiagnosticsScreen> createState() => _DiagnosticsScreenState();
}

class _DiagnosticsScreenState extends State<DiagnosticsScreen> {
  Timer? _timer;
  String? _control;
  bool _busy = false;
  @override
  void initState() {
    super.initState();
    _timer = Timer.periodic(const Duration(seconds: 1), (_) {
      if (mounted) setState(() {});
    });
  }

  @override
  void dispose() {
    _timer?.cancel();
    super.dispose();
  }

  Future<void> _check() async {
    setState(() => _busy = true);
    String result;
    try {
      result = await widget.api.cellularControl();
    } catch (_) {
      result = 'Cellular control request failed';
    }
    if (mounted) {
      setState(() {
        _control = result;
        _busy = false;
      });
    }
  }

  @override
  Widget build(BuildContext context) => Scaffold(
    appBar: AppBar(title: const Text('Diagnostics')),
    body: ListView(
      padding: const EdgeInsets.all(16),
      children: [
        FilledButton(
          onPressed: _busy ? null : _check,
          child: const Text('Check cellular public IPv4'),
        ),
        if (_control != null)
          SelectableText('Direct cellular control: $_control'),
        TextButton(
          onPressed: widget.api.batterySettings,
          child: const Text('Battery optimisation settings'),
        ),
        const SizedBox(height: 16),
        SelectableText(
          const JsonEncoder.withIndent('  ').convert(widget.api.snapshot),
        ),
      ],
    ),
  );
}
