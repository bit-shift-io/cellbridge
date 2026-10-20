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
