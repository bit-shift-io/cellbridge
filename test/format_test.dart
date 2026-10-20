import 'package:cellbridge/src/format.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  group('formatBytes', () {
    test('keeps small values in bytes', () {
      expect(formatBytes(BigInt.zero), '0 B');
      expect(formatBytes(BigInt.from(999)), '999 B');
      // Binary units: 1000 is still below one KiB.
      expect(formatBytes(BigInt.from(1000)), '1000 B');
    });

    test('crosses each binary unit at 1024', () {
      expect(formatBytes(BigInt.from(1024)), '1 KiB');
      expect(formatBytes(BigInt.from(1024 * 1024)), '1 MiB');
      expect(formatBytes(BigInt.from(1024 * 1024 * 1024)), '1 GiB');
    });

    test('renders one decimal below 10 and whole numbers above', () {
      expect(formatBytes(BigInt.from((1.2 * 1024 * 1024).round())), '1.2 MiB');
      expect(formatBytes(BigInt.from(340 * 1024)), '340 KiB');
    });
  });
}
