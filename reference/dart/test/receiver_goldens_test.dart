// The committed receiver goldens (../../goldens/receiver, shared with the
// Rust core and the bindings) must match what this implementation reports.
import 'dart:convert';
import 'dart:io';

import 'package:test/test.dart';

import 'support/receiver_goldens.dart';

void main() {
  test('receiver goldens are up to date', () async {
    const encoder = JsonEncoder.withIndent('  ');
    final goldens = await allGoldens();
    for (final MapEntry(:key, :value) in goldens.entries) {
      final file = File('../../goldens/receiver/$key.json');
      expect(file.existsSync(), isTrue, reason: '$key.json is missing');
      expect(file.readAsStringSync(), '${encoder.convert(value)}\n',
          reason: '$key.json is out of date: '
              'dart run tool/receiver_goldens.dart ../../goldens/receiver');
    }
  });
}
