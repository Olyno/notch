/// Exponential backoff with full jitter for reconnects.
///
/// Full jitter (sleep = rand() * min(cap, base * 2^attempts)) is the
/// AWS-recommended variant: it decorrelates a fleet of phones reconnecting
/// after an outage and still bounds worst-case delay by the cap.
library;

import 'dart:math' as math;

class Backoff {
  Backoff({
    this.base = const Duration(milliseconds: 500),
    this.cap = const Duration(seconds: 30),
    this.min = const Duration(milliseconds: 200),
    math.Random? random,
  }) : _random = random ?? math.Random();

  final Duration base;
  final Duration cap;
  final Duration min;
  final math.Random _random;
  int _attempts = 0;

  /// Successful connection resets the ladder.
  void reset() => _attempts = 0;

  int get attempts => _attempts;

  /// Next sleep, or `Duration.zero` on the very first attempt (connect
  /// immediately after launch).
  Duration next() {
    if (_attempts == 0) {
      _attempts++;
      return Duration.zero;
    }
    final maxUs = math.min(
      base.inMicroseconds * math.pow(2, _attempts - 1),
      cap.inMicroseconds.toDouble(),
    );
    _attempts++;
    // full jitter, never below `min` so we don't hot-loop
    return Duration(
      microseconds: math.max(
        min.inMicroseconds,
        (_random.nextDouble() * maxUs).round(),
      ),
    );
  }
}
