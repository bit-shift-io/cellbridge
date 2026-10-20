// Smoke test for the CellBridge shell and the Rust round-trip wiring.
//
// The Rust library itself is not loaded here; the ping result is injected so
// the widget tree can be exercised without FFI.

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:cellbridge/main.dart';

void main() {
  testWidgets('renders the ping round-trip result', (WidgetTester tester) async {
    await tester.pumpWidget(CellBridgeApp(ping: Future.value('pong')));

    // While the future is pending a spinner is shown.
    expect(find.byType(CircularProgressIndicator), findsOneWidget);

    // Once it resolves the Rust reply is displayed.
    await tester.pump();
    expect(find.text('pong'), findsOneWidget);
    expect(find.byType(CircularProgressIndicator), findsNothing);
  });

  testWidgets('surfaces bridge errors', (WidgetTester tester) async {
    final completer = Completer<String>();
    await tester.pumpWidget(CellBridgeApp(ping: completer.future));

    completer.completeError('boom');
    await tester.pump();
    expect(find.textContaining('boom'), findsOneWidget);
  });
}
