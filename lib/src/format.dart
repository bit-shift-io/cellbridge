/// Formats a byte count using binary (1024-based) units.
///
/// Values below one KiB stay in bytes, values below ten show one decimal
/// (e.g. `1.2 MiB`), and larger values are whole numbers (e.g. `340 KiB`).
String formatBytes(BigInt bytes) {
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB', 'PiB'];

  if (bytes < BigInt.from(1024)) {
    return '$bytes B';
  }

  var value = bytes.toDouble();
  var unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }

  final text = value >= 10
      ? value.toStringAsFixed(0)
      : value.toStringAsFixed(1).replaceFirst(RegExp(r'\.0$'), '');
  return '$text ${units[unit]}';
}

/// Formats a byte-per-second rate as decimal megabits per second.
///
/// One Mbps is 10^6 bits, so a rate of 5,000,000 B/s reads `40`. Values of ten
/// and above are whole numbers; below ten keep one decimal (dropping a trailing
/// `.0`), matching [formatBytes].
String formatMbps(BigInt bytesPerSec) {
  final bits = bytesPerSec * BigInt.from(8);
  final million = BigInt.from(1000000);
  // Round exact decimal halves upward without binary floating-point ties.
  if (bits >= million * BigInt.from(10)) {
    return '${(bits + BigInt.from(500000)) ~/ million}';
  }
  final tenths = (bits * BigInt.from(10) + BigInt.from(500000)) ~/ million;
  final whole = tenths ~/ BigInt.from(10);
  final fraction = tenths % BigInt.from(10);
  return fraction == BigInt.zero ? '$whole' : '$whole.$fraction';
}
