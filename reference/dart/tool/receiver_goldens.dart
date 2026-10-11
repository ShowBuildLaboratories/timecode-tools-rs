// Writes the receiver goldens (see test/support/receiver_goldens.dart).
//
//   dart run tool/receiver_goldens.dart ../../goldens/receiver
import 'dart:convert';
import 'dart:io';

import '../test/support/receiver_goldens.dart';

Future<void> main(List<String> args) async {
  if (args.length != 1) {
    stderr.writeln('usage: dart run tool/receiver_goldens.dart <output dir>');
    exit(2);
  }
  final dir = Directory(args.single)..createSync(recursive: true);
  const encoder = JsonEncoder.withIndent('  ');
  for (final MapEntry(:key, :value) in (await allGoldens()).entries) {
    File('${dir.path}/$key.json')
        .writeAsStringSync('${encoder.convert(value)}\n');
    stdout.writeln('wrote $key.json');
  }
}
