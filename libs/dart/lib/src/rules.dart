/// Local filtering rules: "alert me when a player joins, not when they
/// leave" lives here, evaluated on-device before a notification is built.
/// Server-side tag filters save bandwidth; rules are the source of truth.
library;

import 'model.dart';

enum Effect {
  /// Show a heads-up notification (sound/vibration per priority).
  alert,

  /// Post silently (shade only, no sound) — still recorded.
  silent,

  /// Do nothing at all (deduped, cursor still advances).
  ignore,
}

/// A predicate + effect. Rules evaluate in order; first match wins.
class Rule {
  Rule({
    this.tagsAll,
    this.tagsAny,
    this.tagsNone,
    this.titlePrefix,
    required this.effect,
  });

  /// Convenience: "if tags ⊇ [tags] then [effect]".
  Rule.onTags(List<String> tags, this.effect, {bool not = false})
    : tagsAll = not ? null : tags,
      tagsAny = null,
      tagsNone = not ? tags : null,
      titlePrefix = null;

  final List<String>? tagsAll;
  final List<String>? tagsAny;
  final List<String>? tagsNone;
  final String? titlePrefix;
  final Effect effect;

  bool matches(Message m) {
    if (tagsNone != null && m.tags.any(tagsNone!.contains)) return false;
    if (tagsAll != null && !tagsAll!.every(m.tags.contains)) return false;
    if (tagsAny != null && !m.tags.any(tagsAny!.contains)) return false;
    if (titlePrefix != null && !m.title.startsWith(titlePrefix!)) return false;
    return true;
  }
}

/// Resolution of a message: what to do, and per-message overrides the app
/// passes straight into the Android notification builder.
class Decision {
  Decision({required this.effect, this.priority, this.silent});

  final Effect effect;

  /// Null = use the message's own priority; rules may downgrade e.g. a
  /// 'high' message arriving at night to 'low'.
  final Priority? priority;
  final bool? silent;

  static Decision pass(Message m) =>
      Decision(effect: m.silent ? Effect.silent : Effect.alert);
}

class RuleEngine {
  RuleEngine(this.rules);

  /// Ordered rules; the first match decides. Default when none match:
  /// alert (or silent for `silent` messages).
  final List<Rule> rules;

  Decision decide(Message m) {
    for (final r in rules) {
      if (r.matches(m)) {
        return Decision(
          effect: r.effect,
          priority: r.effect == Effect.alert ? null : m.priority,
          silent: r.effect == Effect.silent,
        );
      }
    }
    return Decision.pass(m);
  }
}
