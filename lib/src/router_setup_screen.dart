import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'bridge.dart';
import 'keys.dart';
import 'router_setup_api.dart';
import 'settings_store.dart';
import 'setup_qr.dart';

/// Credentials live only in these route-owned controllers and are cleared on
/// success, terminal error, cancellation and disposal. Rust owns provisioning.
class RouterSetupScreen extends StatefulWidget {
  const RouterSetupScreen({super.key, required this.store, required this.bridgeRunning,
    this.existingPrivateKey, this.api = const AndroidRouterSetupApi(), this.keyApi});
  final SettingsStore store;
  final bool Function() bridgeRunning;
  final String? existingPrivateKey;
  final RouterSetupApi api;
  final KeyApi? keyApi;

  @override State<RouterSetupScreen> createState() => _RouterSetupScreenState();
}

class _RouterSetupScreenState extends State<RouterSetupScreen> {
  final _form = GlobalKey<FormState>();
  final _host = TextEditingController();
  final _port = TextEditingController(text: '22');
  final _username = TextEditingController(text: 'root');
  final _password = TextEditingController();
  int _priority = 1;
  String _state = 'idle';
  String? _error;
  List<String> _logs = [];
  String? _privateKey;
  String? _publicKey;
  BridgeConfig? _saved;
  Timer? _timer;
  bool _starting = false;
  bool _polling = false;
  bool _confirming = false;
  int _generation = 0;

  bool get _busy => _starting || const {'connecting', 'uploading', 'checkingInternet', 'registering'}.contains(_state);
  KeyApi get _keys => widget.keyApi ?? keyApi;

  @override void initState() {
    super.initState();
    unawaited(_loadGateway());
  }

  Future<void> _loadGateway() async {
    try {
      final address = await widget.api.gateway();
      if (mounted && _host.text.isEmpty) _host.text = address;
    } catch (_) { if (mounted && _host.text.isEmpty) _host.text = '192.168.8.1'; }
  }

  void _clearLogin() { _username.clear(); _password.clear(); }

  Future<void> _attempt() async {
    if (_starting || _busy || !(_form.currentState?.validate() ?? false)) return;
    if (widget.bridgeRunning()) {
      setState(() => _error = 'Disconnect the bridge before router setup.');
      return;
    }
    final generation = ++_generation;
    setState(() { _starting = true; _state = 'connecting'; _error = null; });
    try {
      _privateKey ??= widget.existingPrivateKey ?? await widget.api.readIdentity();
      if (_privateKey == null || _privateKey!.isEmpty) {
        final pair = await _keys.generate();
        _privateKey = pair.privateKey;
      }
      _publicKey = await _keys.derive(_privateKey!);
      // Durable before mutation: a retry after app restart uses the same peer.
      await widget.api.saveIdentity(_privateKey!);
      if (!mounted || generation != _generation) return;
      final endpoint = '${_host.text.trim()}:${_port.text.trim()}';
      final pin = await widget.api.hostPin(endpoint);
      if (!mounted || generation != _generation) return;
      await widget.api.start({
        'host': _host.text.trim(), 'port': int.parse(_port.text),
        'username': _username.text.trim(), 'password': _password.text,
        'priority': _priority, 'phonePublicKey': _publicKey,
        'expectedFingerprint': pin,
      });
      if (!mounted || generation != _generation) return;
      _timer?.cancel();
      _timer = Timer.periodic(const Duration(milliseconds: 400), (_) => unawaited(_poll()));
    } catch (_) {
      if (mounted && generation == _generation) {
        setState(() { _state = 'error'; _error = 'Could not start SSH setup. Check Wi-Fi, router address and port, and disconnect the bridge.'; });
        _clearLogin();
        await widget.api.cancel();
      }
    } finally {
      if (mounted && generation == _generation) setState(() => _starting = false);
    }
    if (mounted && generation == _generation) await _poll();
  }

  Future<void> _poll() async {
    if (!mounted || _starting || _polling) return;
    _polling = true;
    final generation = _generation;
    try {
      final snapshot = await widget.api.snapshot();
      if (!mounted || generation != _generation) return;
      final state = snapshot['state'] as String? ?? 'error';
      setState(() {
        _state = state;
        _logs = (snapshot['logs'] as List? ?? []).whereType<String>().toList();
        _error = snapshot['error'] as String?;
      });
      if (const {'hostKey', 'waitingForInternet', 'success', 'error'}.contains(state)) _timer?.cancel();
      if (state == 'hostKey' && !_confirming) {
        // Reset the guard on EVERY exit path (including savePin failure or an
        // early return), or the form stays locked and can never be retried.
        _confirming = true;
        try {
          final fingerprint = snapshot['fingerprint'] as String?;
          if (fingerprint == null) throw const FormatException('Missing router identity');
          final trusted = await showDialog<bool>(context: context, builder: (context) => AlertDialog(
            title: const Text('Connect to this router?'),
            content: SelectableText('SSH host fingerprint:\n\n$fingerprint\n\nThis identifies the router before your password is sent.'),
            actions: [TextButton(onPressed: () => Navigator.pop(context, false), child: const Text('Cancel')),
              FilledButton(onPressed: () => Navigator.pop(context, true), child: const Text('Connect'))],
          ));
          if (!mounted || generation != _generation) return;
          if (trusted == true) {
            await widget.api.saveHostPin('${_host.text.trim()}:${_port.text.trim()}', fingerprint);
            if (!mounted || generation != _generation) return;
            setState(() => _state = 'idle');
            _polling = false;
            await _attempt();
          } else {
            await widget.api.cancel();
            if (mounted) { _clearLogin(); setState(() => _state = 'idle'); }
          }
        } finally {
          _confirming = false;
        }
      } else if (state == 'success' && _saved == null) {
        final result = snapshot['result'];
        final payload = parseSetupQr('$setupQrPrefix${jsonEncode(result)}');
        if (payload.phonePublicKey != _publicKey || payload.phonePrivateKey != null) {
          throw const FormatException('Unexpected phone identity');
        }
        final config = BridgeConfig(privateKey: _privateKey!, address: payload.address,
          peerPublicKey: payload.peerPublicKey, presharedKey: payload.presharedKey,
          endpoint: payload.endpoint, mtu: payload.mtu);
        if (widget.bridgeRunning()) throw StateError('Bridge started during setup');
        await widget.store.save(config);
        if (!mounted || generation != _generation) return;
        _clearLogin();
        setState(() => _saved = config);
      } else if (state == 'error') {
        _clearLogin();
        await widget.api.cancel();
      }
    } catch (_) {
      _timer?.cancel();
      if (mounted && generation == _generation) {
        _clearLogin();
        setState(() { _state = 'error'; _error = 'Could not complete setup or save the returned settings. Copy the output for troubleshooting; retry reuses this phone.'; });
        await widget.api.cancel();
      }
    } finally { _polling = false; }
  }

  @override void dispose() {
    _generation++;
    _timer?.cancel();
    unawaited(widget.api.cancel().catchError((Object _) {}));
    _clearLogin();
    for (final controller in [_host, _port, _username, _password]) { controller.dispose(); }
    super.dispose();
  }

  String get _status => switch (_state) {
    'connecting' => 'Connecting over SSH…',
    'uploading' => 'Uploading router scripts…',
    'checkingInternet' => 'Checking router internet…',
    'registering' => 'Configuring this phone…',
    'waitingForInternet' => 'Waiting for router internet',
    'success' => _saved == null ? 'Saving settings…' : 'Router configured. Settings saved.',
    'error' => 'Setup needs attention',
    _ => 'Set up CellBridge on your router',
  };

  @override Widget build(BuildContext context) {
    final locked = _busy || _confirming || _saved != null;
    return Scaffold(
      appBar: AppBar(title: const Text('Set up router')),
      body: SingleChildScrollView(padding: const EdgeInsets.all(16), child: Form(key: _form,
        child: Column(crossAxisAlignment: CrossAxisAlignment.stretch, children: [
          const Text('Join the router’s Wi-Fi. Enter its SSH login for this setup only; the username and password are not saved.'),
          const SizedBox(height: 12),
          TextFormField(key: const Key('router-host'), controller: _host, enabled: !locked,
            decoration: const InputDecoration(labelText: 'Router address'), autocorrect: false,
            validator: (value) => value == null || value.trim().isEmpty || value.contains(RegExp(r'\s')) ? 'Enter the router address' : null),
          TextFormField(key: const Key('router-port'), controller: _port, enabled: !locked,
            decoration: const InputDecoration(labelText: 'SSH port'), keyboardType: TextInputType.number,
            validator: (value) { final port = int.tryParse(value ?? ''); return port == null || port < 1 || port > 65535 ? 'Enter a port from 1 to 65535' : null; }),
          TextFormField(key: const Key('router-username'), controller: _username, enabled: !locked,
            decoration: const InputDecoration(labelText: 'SSH username'), autocorrect: false, enableSuggestions: false,
            enableIMEPersonalizedLearning: false,
            validator: (value) => value == null || value.trim().isEmpty ? 'Enter the SSH username' : null),
          TextFormField(key: const Key('router-password'), controller: _password, enabled: !locked,
            decoration: const InputDecoration(labelText: 'SSH password'), obscureText: true, autocorrect: false,
            enableSuggestions: false, enableIMEPersonalizedLearning: false,
            validator: (value) => value == null || value.isEmpty ? 'Enter the SSH password' : null),
          DropdownButtonFormField<int>(initialValue: _priority, decoration: const InputDecoration(labelText: 'Phone priority (1 is highest)'),
            items: [for (var value = 1; value <= 10; value++) DropdownMenuItem(value: value, child: Text('$value'))],
            onChanged: locked ? null : (value) => setState(() => _priority = value!)),
          const SizedBox(height: 16),
          Text(_status, style: Theme.of(context).textTheme.titleMedium),
          if (_busy) const LinearProgressIndicator(),
          if (_error != null) Padding(padding: const EdgeInsets.only(top: 8), child: Text(_error!, style: TextStyle(color: Theme.of(context).colorScheme.error))),
          if (_state == 'waitingForInternet') const Text('Restore the router’s internet connection, then tap Retry. No registration is performed while it is offline.'),
          if (_logs.isNotEmpty) ...[
            const SizedBox(height: 12),
            const Text('Script output'),
            Container(padding: const EdgeInsets.all(12), color: Theme.of(context).colorScheme.surfaceContainerHighest,
              child: SelectableText(_logs.join('\n'), style: const TextStyle(fontFamily: 'monospace', fontSize: 12))),
            TextButton.icon(onPressed: () async { await Clipboard.setData(ClipboardData(text: _logs.join('\n'))); },
              icon: const Icon(Icons.copy), label: const Text('Copy output')),
          ],
        ]))),
      bottomNavigationBar: SafeArea(child: Padding(padding: const EdgeInsets.all(16), child: Row(children: [
        TextButton(onPressed: () => Navigator.of(context).pop(_saved), child: Text(_saved == null ? 'Cancel' : 'Done')),
        const Spacer(),
        if (_saved == null) FilledButton(onPressed: locked ? null : _attempt,
          child: Text(_state == 'waitingForInternet' || _state == 'error' ? 'Retry' : 'Set up this phone')),
      ]))),
    );
  }
}
