import 'package:flutter/material.dart';

import 'bridge.dart';
import 'settings_store.dart';

/// A form over [BridgeConfig].
///
/// The screen owns the text controllers and the validation UX; persistence is
/// delegated to the injected [SettingsStore]. Key material is never printed in
/// an error message — validation messages describe the *shape* of the value,
/// not the value itself.
class ConfigScreen extends StatefulWidget {
  const ConfigScreen({
    super.key,
    required this.store,
    this.initialConfig,
    required this.bridgeRunning,
    this.onSaved,
  });

  /// Where a validated configuration is written.
  final SettingsStore store;

  /// The configuration to pre-fill the form with, if any.
  final BridgeConfig? initialConfig;

  /// Whether the bridge is currently up. Saving is refused while it is, so a
  /// live tunnel is never reconfigured underneath the user.
  final bool Function() bridgeRunning;

  /// Called with the saved configuration after a successful write.
  final ValueChanged<BridgeConfig>? onSaved;

  @override
  State<ConfigScreen> createState() => _ConfigScreenState();
}

class _ConfigScreenState extends State<ConfigScreen> {
  final _formKey = GlobalKey<FormState>();

  late final TextEditingController _privateKey;
  late final TextEditingController _address;
  late final TextEditingController _peerPublicKey;
  late final TextEditingController _presharedKey;
  late final TextEditingController _endpoint;
  late final TextEditingController _mtu;

  bool _saving = false;

  @override
  void initState() {
    super.initState();
    final config = widget.initialConfig;
    _privateKey = TextEditingController(text: config?.privateKey ?? '');
    _address = TextEditingController(text: config?.address ?? '');
    _peerPublicKey = TextEditingController(text: config?.peerPublicKey ?? '');
    _presharedKey = TextEditingController(text: config?.presharedKey ?? '');
    _endpoint = TextEditingController(text: config?.endpoint ?? '');
    _mtu = TextEditingController(text: config == null ? '' : '${config.mtu}');
  }

  @override
  void dispose() {
    _privateKey.dispose();
    _address.dispose();
    _peerPublicKey.dispose();
    _presharedKey.dispose();
    _endpoint.dispose();
    _mtu.dispose();
    super.dispose();
  }

  Future<void> _save() async {
    // Refuse to reconfigure a running tunnel. (The alternative would be to stop
    // the bridge first; refusing keeps the user in control of their session.)
    if (widget.bridgeRunning()) {
      _showMessage('Disconnect the bridge before editing the configuration.');
      return;
    }

    if (!(_formKey.currentState?.validate() ?? false)) {
      return;
    }

    final config = BridgeConfig(
      privateKey: _privateKey.text.trim(),
      address: _address.text.trim(),
      peerPublicKey: _peerPublicKey.text.trim(),
      presharedKey: _presharedKey.text.trim().isEmpty
          ? null
          : _presharedKey.text.trim(),
      endpoint: _endpoint.text.trim(),
      mtu: int.parse(_mtu.text.trim()),
    );

    setState(() => _saving = true);
    try {
      await widget.store.save(config);
      if (!mounted) {
        return;
      }
      widget.onSaved?.call(config);
      _showMessage('Configuration saved.');
    } catch (error) {
      if (!mounted) {
        return;
      }
      // Deliberately generic: a backend error must not leak key material.
      debugPrint('ConfigScreen failed to save (${error.runtimeType}).');
      _showMessage('Could not save configuration. Please try again.');
    } finally {
      if (mounted) {
        setState(() => _saving = false);
      }
    }
  }

  void _showMessage(String message) {
    final messenger = ScaffoldMessenger.maybeOf(context);
    messenger
      ?..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(content: Text(message)));
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('Configuration')),
      body: Form(
        key: _formKey,
        child: ListView(
          padding: const EdgeInsets.all(16),
          children: [
            TextFormField(
              key: const Key('config-private-key'),
              controller: _privateKey,
              obscureText: true,
              autocorrect: false,
              enableSuggestions: false,
              decoration: const InputDecoration(
                labelText: 'Private key',
                helperText: 'Base64 WireGuard private key',
              ),
              validator: (value) => validatePrivateKey(value ?? ''),
            ),
            TextFormField(
              key: const Key('config-address'),
              controller: _address,
              autocorrect: false,
              enableSuggestions: false,
              decoration: const InputDecoration(
                labelText: 'Address',
                helperText: 'Interface address in CIDR form',
              ),
              validator: (value) => validateAddress(value ?? ''),
            ),
            TextFormField(
              key: const Key('config-peer-public-key'),
              controller: _peerPublicKey,
              autocorrect: false,
              enableSuggestions: false,
              decoration: const InputDecoration(
                labelText: 'Peer public key',
                helperText: 'Base64 WireGuard public key of the LAN exit peer',
              ),
              validator: (value) => validatePeerPublicKey(value ?? ''),
            ),
            TextFormField(
              key: const Key('config-preshared-key'),
              controller: _presharedKey,
              obscureText: true,
              autocorrect: false,
              enableSuggestions: false,
              decoration: const InputDecoration(
                labelText: 'Preshared key (optional)',
                helperText: 'Base64 WireGuard preshared key',
              ),
              validator: validatePresharedKey,
            ),
            TextFormField(
              key: const Key('config-endpoint'),
              controller: _endpoint,
              autocorrect: false,
              enableSuggestions: false,
              decoration: const InputDecoration(
                labelText: 'Endpoint',
                helperText: 'host:port, e.g. 203.0.113.1:51820',
              ),
              validator: (value) => validateEndpoint(value ?? ''),
            ),
            TextFormField(
              key: const Key('config-mtu'),
              controller: _mtu,
              keyboardType: TextInputType.number,
              decoration: const InputDecoration(
                labelText: 'MTU',
                helperText: 'Between $minMtu and $maxMtu',
              ),
              validator: (value) => validateMtu(value ?? ''),
            ),
            const SizedBox(height: 24),
            Row(
              mainAxisAlignment: MainAxisAlignment.end,
              children: [
                TextButton(
                  key: const Key('config-cancel'),
                  onPressed: _saving
                      ? null
                      : () => Navigator.of(context).maybePop(),
                  child: const Text('Cancel'),
                ),
                const SizedBox(width: 12),
                FilledButton(
                  key: const Key('config-save'),
                  onPressed: _saving ? null : _save,
                  child: const Text('Save'),
                ),
              ],
            ),
          ],
        ),
      ),
    );
  }
}
