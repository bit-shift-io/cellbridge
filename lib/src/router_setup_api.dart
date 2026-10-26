import 'dart:convert';
import 'package:flutter/services.dart';

/// Thin skin over Android socket/storage services and the Rust setup job.
/// SSH credentials only cross [start]; there is no credential-storage API.
abstract class RouterSetupApi {
  Future<String> gateway();
  Future<String?> readIdentity();
  Future<void> saveIdentity(String key);
  Future<String?> hostPin(String endpoint);
  Future<void> saveHostPin(String endpoint, String fingerprint);
  Future<void> start(Map<String, Object?> input);
  Future<Map<String, dynamic>> snapshot();
  Future<void> cancel();
}

class AndroidRouterSetupApi implements RouterSetupApi {
  const AndroidRouterSetupApi({this.channel = const MethodChannel('dev.cellbridge/router-setup')});
  final MethodChannel channel;
  @override Future<String> gateway() async => await channel.invokeMethod<String>('gateway') ?? '192.168.8.1';
  @override Future<String?> readIdentity() => channel.invokeMethod<String>('identity');
  @override Future<void> saveIdentity(String key) => channel.invokeMethod<void>('saveIdentity', {'privateKey': key});
  @override Future<String?> hostPin(String endpoint) => channel.invokeMethod<String>('hostPin', {'endpoint': endpoint});
  @override Future<void> saveHostPin(String endpoint, String fingerprint) => channel.invokeMethod<void>('saveHostPin', {'endpoint': endpoint, 'fingerprint': fingerprint});
  @override Future<void> start(Map<String, Object?> input) => channel.invokeMethod<void>('start', input);
  @override Future<Map<String, dynamic>> snapshot() async {
    final text = await channel.invokeMethod<String>('snapshot');
    return jsonDecode(text ?? '{"state":"idle","logs":[]}') as Map<String, dynamic>;
  }
  @override Future<void> cancel() => channel.invokeMethod<void>('cancel');
}
