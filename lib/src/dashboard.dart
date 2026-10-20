import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';

import 'bridge.dart';
import 'format.dart';

/// The bridge dashboard: lifecycle toggle, live counters and the last error.
///
/// The widget deliberately does not know where the configuration comes from;
/// [config] supplies it at connect time so the settings store can be wired in
/// without touching the UI.
class Dashboard extends StatefulWidget {
  const Dashboard({
    super.key,
    required this.config,
    this.actions = const [],
    this.pollInterval = const Duration(milliseconds: 500),
  });

  /// Returns the configuration to start with, or `null` when none is saved.
  final BridgeConfig? Function() config;

  /// Optional app-bar actions (the demo uses this to trigger an error).
  final List<Widget> actions;

  /// How often to read status and stats while the bridge is not idle.
  final Duration pollInterval;

  @override
  State<Dashboard> createState() => _DashboardState();
}

class _DashboardState extends State<Dashboard> with WidgetsBindingObserver {
  BridgeStatus _status = const BridgeStatus(state: BridgeState.idle);
  Stats _stats = Stats(
    txBytes: BigInt.zero,
    rxBytes: BigInt.zero,
    activeFlows: BigInt.zero,
  );

  Timer? _pollTimer;
  bool _busy = false;
  String? _actionError;

  bool _tickerVisible = true;
  bool _appVisible = true;
  ValueListenable<TickerModeData>? _tickerMode;

  bool get _visible => _tickerVisible && _appVisible;

  /// A thrown start/stop error, unless the bridge itself reported one.
  String? get _errorText => _status.error ?? _actionError;

  bool get _isOn =>
      _status.state == BridgeState.connected ||
      _status.state == BridgeState.error;

  bool get _canToggle => !_busy && _status.state != BridgeState.connecting;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    _status = bridgeApi.status();
    _stats = bridgeApi.stats();
    _syncPolling();
  }

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    final notifier = TickerMode.getValuesNotifier(context);
    if (!identical(notifier, _tickerMode)) {
      _tickerMode?.removeListener(_onTickerVisibilityChanged);
      _tickerMode = notifier;
      _tickerMode!.addListener(_onTickerVisibilityChanged);
      _tickerVisible = notifier.value.enabled;
      _syncPolling();
    }
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    _appVisible = state == AppLifecycleState.resumed;
    _syncPolling();
  }

  void _onTickerVisibilityChanged() {
    final visible = _tickerMode?.value.enabled ?? true;
    if (visible == _tickerVisible) {
      return;
    }
    _tickerVisible = visible;
    _syncPolling();
  }

  @override
  void dispose() {
    _pollTimer?.cancel();
    _tickerMode?.removeListener(_onTickerVisibilityChanged);
    WidgetsBinding.instance.removeObserver(this);
    super.dispose();
  }

  /// Keeps the timer running only while visible and not idle.
  void _syncPolling() {
    final shouldPoll = _visible && _status.state != BridgeState.idle;
    if (!shouldPoll) {
      _pollTimer?.cancel();
      _pollTimer = null;
      return;
    }
    _pollTimer ??= Timer.periodic(widget.pollInterval, (_) => _poll());
  }

  void _poll() {
    if (!mounted) {
      return;
    }
    final nextStatus = bridgeApi.status();
    final nextStats = bridgeApi.stats();
    final changed = nextStatus != _status || nextStats != _stats;
    if (changed) {
      setState(() {
        _status = nextStatus;
        _stats = nextStats;
      });
    }
    if (nextStatus.state == BridgeState.idle) {
      _syncPolling();
    }
  }

  void _readNow() {
    _status = bridgeApi.status();
    _stats = bridgeApi.stats();
  }

  Future<void> _toggle() async {
    if (_isOn) {
      await _stop();
    } else {
      await _start();
    }
  }

  Future<void> _start() async {
    final config = widget.config();
    if (config == null) {
      setState(() {
        _actionError = 'No configuration saved yet.';
      });
      return;
    }

    setState(() {
      _busy = true;
      _actionError = null;
    });

    try {
      final starting = bridgeApi.start(config);
      // The backend flips to Connecting synchronously, so show that state
      // while the start call is still in flight.
      setState(_readNow);
      _syncPolling();
      await starting;
      if (!mounted) {
        return;
      }
      setState(() {
        _busy = false;
        _readNow();
      });
      _syncPolling();
    } catch (error) {
      if (!mounted) {
        return;
      }
      setState(() {
        _busy = false;
        _actionError = 'Could not connect: $error';
      });
    }
  }

  Future<void> _stop() async {
    setState(() {
      _busy = true;
      _actionError = null;
    });

    try {
      await bridgeApi.stop();
      if (!mounted) {
        return;
      }
      setState(() {
        _busy = false;
        _readNow();
      });
      _syncPolling();
    } catch (error) {
      if (!mounted) {
        return;
      }
      setState(() {
        _busy = false;
        _actionError = 'Could not disconnect: $error';
      });
    }
  }

  String get _stateLabel => switch (_status.state) {
    BridgeState.idle => 'Idle',
    BridgeState.connecting => 'Connecting',
    BridgeState.connected => 'Connected',
    BridgeState.error => 'Error',
  };

  @override
  Widget build(BuildContext context) {
    final errorText = _errorText;
    return Scaffold(
      appBar: AppBar(title: const Text('CellBridge'), actions: widget.actions),
      body: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Card(
              child: ListTile(
                leading: Icon(_isOn ? Icons.vpn_lock : Icons.vpn_lock_outlined),
                title: Text(
                  _stateLabel,
                  key: const Key('status-text'),
                  style: Theme.of(context).textTheme.titleLarge,
                ),
              ),
            ),
            const SizedBox(height: 16),
            FilledButton.icon(
              key: const Key('connect-toggle'),
              onPressed: _canToggle ? _toggle : null,
              icon: Icon(_isOn ? Icons.link_off : Icons.link),
              label: Text(_isOn ? 'Disconnect' : 'Connect'),
            ),
            const SizedBox(height: 16),
            Card(
              child: Column(
                children: [
                  _StatRow(
                    label: 'In',
                    value: formatBytes(_stats.rxBytes),
                    valueKey: 'bytes-in',
                  ),
                  const Divider(height: 1),
                  _StatRow(
                    label: 'Out',
                    value: formatBytes(_stats.txBytes),
                    valueKey: 'bytes-out',
                  ),
                  const Divider(height: 1),
                  _StatRow(
                    label: 'Active flows',
                    value: '${_stats.activeFlows}',
                    valueKey: 'active-flows',
                  ),
                ],
              ),
            ),
            if (errorText != null) ...[
              const SizedBox(height: 16),
              Card(
                color: Theme.of(context).colorScheme.errorContainer,
                child: Padding(
                  padding: const EdgeInsets.all(16),
                  child: Text(
                    errorText,
                    key: const Key('last-error'),
                    style: TextStyle(
                      color: Theme.of(context).colorScheme.onErrorContainer,
                    ),
                  ),
                ),
              ),
            ],
          ],
        ),
      ),
    );
  }
}

class _StatRow extends StatelessWidget {
  const _StatRow({
    required this.label,
    required this.value,
    required this.valueKey,
  });

  final String label;
  final String value;
  final String valueKey;

  @override
  Widget build(BuildContext context) {
    return ListTile(
      title: Text(label),
      trailing: Text(
        value,
        key: Key(valueKey),
        style: Theme.of(context).textTheme.titleMedium,
      ),
    );
  }
}
