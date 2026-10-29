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

  group('formatMbps', () {
    test('renders whole numbers at and above ten', () {
      expect(formatMbps(BigInt.from(5000000)), '40'); // 40 Mbps
      expect(formatMbps(BigInt.from(1250000)), '10'); // exactly 10 Mbps
    });

    test('renders one decimal below ten, dropping a trailing .0', () {
      expect(formatMbps(BigInt.from(425000)), '3.4');
      expect(formatMbps(BigInt.from(125000)), '1');
    });

    test('zero is zero', () {
      expect(formatMbps(BigInt.zero), '0');
    });
    test('decimal ties round up consistently across skins', () {
      expect(formatMbps(BigInt.from(18750)), '0.2');
      expect(formatMbps(BigInt.from(118750)), '1');
      expect(formatMbps(BigInt.from(1243750)), '10');
      expect(formatMbps(BigInt.from(50000)), '0.4');
      expect(formatMbps(BigInt.parse('18446744073709551615')), '147573952589676');
    });
  });
}
