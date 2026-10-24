import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import 'bridge.dart';
import 'keys.dart';
import 'settings_store.dart';
import 'setup_qr.dart';

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
    this.keyApi,
    this.testConnection,
    this.scanSetupQr,
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

  /// Key operations. Defaults to the process-wide [keyApi]; widget tests inject
  /// a fake so they never need the Rust library.
  final KeyApi? keyApi;
  final Future<void> Function(BridgeConfig)? testConnection;

  /// Opens the QR scanner and returns its decoded text, or null if cancelled.
  ///
  /// Non-null only on Android; tests inject a fake.
  final Future<String?> Function(BuildContext context)? scanSetupQr;

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

  /// Read-only display of the key derived from [_privateKey].
  late final TextEditingController _publicKey;

  static const _hintEmpty =
      'Generate or paste a private key to show its public key.';
  static const _hintInvalid =
      'Enter a valid private key to see its public key.';
  static const _hintDeriving = 'Deriving public key…';
  static const _hintReady = 'Add this as the peer on your router.';

  String _publicKeyHint = _hintEmpty;

  Timer? _deriveDebounce;
  int _deriveRequest = 0;

  /// Guards the controller listener while [_generateKeypair] fills the field,
  /// so generating does not schedule a redundant derivation.
  bool _suppressPrivateKeyListener = false;

  bool _saving = false;
  bool _scanning = false;
  bool _testing = false;
  String? _testResult;
  int _testRevision = 0;

  void _invalidateTestResult() {
    _testRevision++;
    if (mounted && _testResult != null) setState(() => _testResult = null);
  }

  Future<void> _testConnection() async {
    if (widget.bridgeRunning()) {
      _showMessage('Disconnect the bridge before testing the connection.');
      return;
    }
    if (!(_formKey.currentState?.validate() ?? false)) return;
    final config = BridgeConfig(
      privateKey: _privateKey.text.trim(),
      address: _address.text.trim(),
      peerPublicKey: _peerPublicKey.text.trim(),
      endpoint: _endpoint.text.trim(),
      mtu: int.parse(_mtu.text.trim()),
      presharedKey: _presharedKey.text.trim().isEmpty
          ? null
          : _presharedKey.text.trim(),
    );
    setState(() {
      _testing = true;
      _testResult = null;
    });
    final testedRevision = _testRevision;
    try {
      await widget.testConnection!(config);
      if (mounted && testedRevision == _testRevision) {
        setState(
          () => _testResult =
              'Authenticated WireGuard handshake succeeded over Wi-Fi.',
        );
      }
    } catch (_) {
      if (mounted && testedRevision == _testRevision) {
        setState(
          () => _testResult = 'Wi-Fi connection test failed. Check Wi-Fi, endpoint and paired keys.',
        );
      }
    } finally {
      if (mounted) setState(() => _testing = false);
    }
  }

  KeyApi get _keys => widget.keyApi ?? keyApi;

  Future<void> _scanSetupQr() async {
    if (_saving || _scanning) {
      return;
    }
    setState(() => _scanning = true);
    try {
      final raw = await widget.scanSetupQr!(context);
      if (!mounted || raw == null) {
        return;
      }
      try {
        final payload = parseSetupQr(raw);
        _address.text = payload.address;
        _peerPublicKey.text = payload.peerPublicKey;
        _presharedKey.text = payload.presharedKey ?? '';
        _endpoint.text = payload.endpoint;
        _mtu.text = '${payload.mtu}';
        _invalidateTestResult();

        final privateKey = _privateKey.text.trim();
        if (privateKey.isEmpty) {
          _showMessage(
            'Router settings loaded. Generate a key pair and re-run router setup with its public key.',
          );
        } else if (payload.phonePublicKey == null) {
          _showMessage('Router settings loaded. Review, then Save.');
        } else {
          // Refresh the derived key before comparing: the displayed value may
          // be stale while a debounce is pending, and a failed derivation must
          // not look like a match.
          _deriveDebounce?.cancel();
          await _derivePublicKey(privateKey);
          if (!mounted) {
            return;
          }
          final currentPublicKey = _publicKey.text.trim();
          if (currentPublicKey.isEmpty) {
            _showMessage(
              'Router settings loaded, but the app could not verify the phone key. Re-run router setup with the public key for this phone.',
            );
          } else if (payload.phonePublicKey != currentPublicKey) {
            _showMessage(
              'Router settings loaded, but this router expects a different phone key. Re-run router setup with the public key shown above.',
            );
          } else {
            _showMessage('Router settings loaded. Review, then Save.');
          }
        }
      } on FormatException {
        _showMessage('That QR code is not a CellBridge setup code.');
      }
    } finally {
      if (mounted) {
        setState(() => _scanning = false);
      }
    }
  }

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
    _publicKey = TextEditingController();
    for (final field in [
      _privateKey,
      _address,
      _peerPublicKey,
      _presharedKey,
      _endpoint,
      _mtu,
    ]) {
      field.addListener(_invalidateTestResult);
    }

    _privateKey.addListener(_onPrivateKeyChanged);
    final initialPrivateKey = _privateKey.text.trim();
    if (initialPrivateKey.isNotEmpty) {
      WidgetsBinding.instance.addPostFrameCallback(
        (_) => _derivePublicKey(initialPrivateKey),
      );
    }
  }

  @override
  void dispose() {
    _deriveDebounce?.cancel();
    _privateKey.dispose();
    _address.dispose();
    _peerPublicKey.dispose();
    _presharedKey.dispose();
    _endpoint.dispose();
    _mtu.dispose();
    _publicKey.dispose();
    super.dispose();
  }

  /// Debounced derivation while the user types or pastes a private key.
  void _onPrivateKeyChanged() {
    if (_suppressPrivateKeyListener) {
      return;
    }
    _deriveDebounce?.cancel();
    final value = _privateKey.text.trim();
    if (value.isEmpty) {
      _clearPublicKey(_hintEmpty);
      return;
    }
    if (validatePrivateKey(value) != null) {
      // Shape-invalid text is not an error here: the form validator already
      // owns that, and red error text while typing would be hostile.
      _clearPublicKey(_hintInvalid);
      return;
    }
    setState(() => _publicKeyHint = _hintDeriving);
    _deriveDebounce = Timer(
      const Duration(milliseconds: 300),
      () => _derivePublicKey(value),
    );
  }

  void _clearPublicKey(String hint) {
    _deriveRequest++;
    if (!mounted) {
      return;
    }
    setState(() {
      _publicKey.clear();
      _publicKeyHint = hint;
    });
  }

  /// Derives and displays the public key for [privateKey].
  ///
  /// A malformed key (shape-valid but not canonical base64, or the wrong
  /// length) falls back to the neutral hint; nothing here is a form error.
  Future<void> _derivePublicKey(String privateKey) async {
    if (!mounted) {
      return;
    }
    final value = privateKey.trim();
    if (validatePrivateKey(value) != null) {
      _clearPublicKey(_hintInvalid);
      return;
    }

    final request = ++_deriveRequest;
    setState(() => _publicKeyHint = _hintDeriving);
    try {
      final publicKey = await _keys.derive(value);
      if (!mounted || request != _deriveRequest) {
        return;
      }
      setState(() {
        _publicKey.text = publicKey;
        _publicKeyHint = _hintReady;
      });
    } catch (_) {
      if (!mounted || request != _deriveRequest) {
        return;
      }
      setState(() {
        _publicKey.clear();
        _publicKeyHint = _hintInvalid;
      });
    }
  }

  Future<void> _generateKeypair() async {
    try {
      final pair = await _keys.generate();
      if (!mounted) {
        return;
      }
      _deriveDebounce?.cancel();
      _deriveRequest++;
      setState(() {
        _suppressPrivateKeyListener = true;
        _privateKey.text = pair.privateKey;
        _suppressPrivateKeyListener = false;
        _publicKey.text = pair.publicKey;
        _publicKeyHint = _hintReady;
      });
    } catch (_) {
      if (!mounted) {
        return;
      }
      _showMessage('Could not generate a key pair. Please try again.');
    }
  }

  Future<void> _copyPublicKey() async {
    final publicKey = _publicKey.text;
    if (publicKey.isEmpty) {
      return;
    }
    await Clipboard.setData(ClipboardData(text: publicKey));
    if (!mounted) {
      return;
    }
    _showMessage('Phone public key copied.');
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

    // Refresh the displayed public key on save, in case the user typed and hit
    // Save before the debounce fired.
    _deriveDebounce?.cancel();
    await _derivePublicKey(_privateKey.text.trim());
    if (!mounted) {
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
        child: SingleChildScrollView(
          padding: const EdgeInsets.all(16),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              if (widget.scanSetupQr != null) ...[
                OutlinedButton.icon(
                  key: const Key('config-scan-qr'),
                  onPressed: _saving || _scanning ? null : _scanSetupQr,
                  icon: const Icon(Icons.qr_code_scanner),
                  label: Text(_scanning ? 'Opening camera…' : 'Scan router QR'),
                ),
                const SizedBox(height: 8),
              ],
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
              Align(
                alignment: Alignment.centerLeft,
                child: OutlinedButton.icon(
                  key: const Key('config-generate-keypair'),
                  onPressed: _saving ? null : _generateKeypair,
                  icon: const Icon(Icons.vpn_key_outlined),
                  label: const Text('Generate key pair'),
                ),
              ),
              const SizedBox(height: 8),
              TextFormField(
                key: const Key('config-public-key'),
                controller: _publicKey,
                readOnly: true,
                enableInteractiveSelection: true,
                autocorrect: false,
                enableSuggestions: false,
                decoration: InputDecoration(
                  labelText: 'Phone public key',
                  helperText: _publicKeyHint,
                  helperMaxLines: 2,
                  hintText: 'No public key yet',
                  suffixIcon: IconButton(
                    key: const Key('config-copy-public-key'),
                    tooltip: 'Copy phone public key',
                    onPressed: _publicKey.text.isEmpty ? null : _copyPublicKey,
                    icon: const Icon(Icons.copy),
                  ),
                ),
              ),
              const SizedBox(height: 8),
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
                  helperText:
                      'Base64 WireGuard public key of the LAN exit peer',
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
              const SizedBox(height: 8),
              if (widget.testConnection != null) ...[
                OutlinedButton.icon(
                  key: const Key('config-test-connection'),
                  onPressed: _testing || _saving ? null : _testConnection,
                  icon: const Icon(Icons.network_check),
                  label: Text(
                    _testing
                        ? 'Testing Wi-Fi connection…'
                        : 'Test Wi-Fi connection',
                  ),
                ),
                const Text(
                  'Tests the WireGuard handshake only. No SIM or mobile data required. Does not save changes.',
                ),
                if (_testResult != null) Text(_testResult!),
              ],
            ],
          ),
        ),
      ),
      bottomNavigationBar: SafeArea(
        child: Padding(
          padding: const EdgeInsets.fromLTRB(16, 8, 16, 16),
          child: Row(
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
        ),
      ),
    );
  }
}
