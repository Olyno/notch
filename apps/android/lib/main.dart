/// Notch phone app: configure the relay + rules, keep the listener alive.
///
/// Everything realtime happens in a foreground service running its own
/// Dart entrypoint (service_entry.dart); this UI only reads/writes the
/// config file and commands the service.
library;

import 'package:flutter/material.dart';
import 'package:notch/notch.dart';

import 'config.dart';
import 'notify_bridge.dart';

void main() => runApp(const NotchApp());

class NotchApp extends StatelessWidget {
  const NotchApp({super.key});

  @override
  Widget build(BuildContext context) => MaterialApp(
    title: 'Notch',
    theme: ThemeData(
      colorSchemeSeed: Colors.indigo,
      useMaterial3: true,
      brightness: Brightness.dark,
    ),
    home: const HomePage(),
  );
}

class HomePage extends StatefulWidget {
  const HomePage({super.key});

  @override
  State<HomePage> createState() => _HomePageState();
}

class _HomePageState extends State<HomePage> {
  late final TextEditingController _relay;
  late final TextEditingController _topic;
  AppConfig _config = AppConfig();
  bool _running = false;
  bool _ready = false;
  String? _error;

  @override
  void initState() {
    super.initState();
    _relay = TextEditingController();
    _topic = TextEditingController();
    _init();
  }

  Future<void> _init() async {
    final dir = await UiChannel.filesDir();
    final config = await AppConfig.load(dir);
    _relay.text = config.relay;
    _topic.text = config.topic;
    setState(() {
      _config = config;
      _ready = true;
    });
    final running = await UiChannel.isRunning();
    setState(() => _running = running);
  }

  Future<void> _save() async {
    final dir = await UiChannel.filesDir();
    final cfg = AppConfig(
      relay: _relay.text.trim(),
      topic: _topic.text.trim().isEmpty ? 'alerts' : _topic.text.trim(),
      rules: _config.rules,
      serverFilterNone: _config.serverFilterNone,
    );
    await cfg.save(dir);
    setState(() => _config = cfg);
    if (_running) await _restart(); // service re-reads config on boot
  }

  Future<void> _toggle() async {
    if (_running) {
      await UiChannel.stopService();
      setState(() => _running = false);
      return;
    }
    final granted = await UiChannel.requestNotifications();
    if (!granted) {
      setState(() => _error = 'POST_NOTIFICATIONS denied — enable it in system settings');
      return;
    }
    await UiChannel.requestBatteryExemption(); // best effort
    await _save();
    await UiChannel.startService();
    setState(() {
      _running = true;
      _error = null;
    });
  }

  Future<void> _restart() async {
    await UiChannel.stopService();
    await UiChannel.startService();
  }

  void _addRule() {
    showModalBottomSheet<void>(
      context: context,
      builder: (sheetCtx) => RuleEditor(
        onSave: (rule) {
          setState(() => _config = AppConfig(
            relay: _config.relay,
            topic: _config.topic,
            serverFilterNone: _config.serverFilterNone,
            rules: [..._config.rules, rule],
          ));
        },
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    if (!_ready) return const Scaffold(body: Center(child: CircularProgressIndicator()));
    final theme = Theme.of(context);
    return Scaffold(
      appBar: AppBar(
        title: const Text('Notch'),
        actions: [
          IconButton(
            icon: const Icon(Icons.add),
            tooltip: 'Add rule',
            onPressed: _addRule,
          ),
        ],
      ),
      body: ListView(
        padding: const EdgeInsets.all(16),
        children: [
          TextField(
            controller: _relay,
            decoration: const InputDecoration(
              labelText: 'Relay URL',
              helperText: 'http:// on your LAN, e.g. http://192.168.1.10:8899',
              border: OutlineInputBorder(),
            ),
            keyboardType: TextInputType.url,
          ),
          const SizedBox(height: 12),
          TextField(
            controller: _topic,
            decoration: const InputDecoration(
              labelText: 'Topic',
              border: OutlineInputBorder(),
            ),
          ),
          const SizedBox(height: 8),
          Align(
            alignment: Alignment.centerLeft,
            child: FilledButton.tonal(onPressed: _save, child: const Text('Save')),
          ),
          const SizedBox(height: 24),
          Row(
            children: [
              Icon(
                _running ? Icons.notifications_active : Icons.notifications_off,
                color: _running ? Colors.greenAccent : Colors.grey,
              ),
              const SizedBox(width: 8),
              Text(
                _running ? 'Listener running' : 'Listener stopped',
                style: theme.textTheme.titleMedium,
              ),
              const Spacer(),
              FilledButton(
                onPressed: _toggle,
                child: Text(_running ? 'Stop' : 'Start'),
              ),
            ],
          ),
          if (_error != null) ...[
            const SizedBox(height: 8),
            Text(_error!, style: TextStyle(color: theme.colorScheme.error)),
          ],
          const SizedBox(height: 24),
          Text('Alert rules', style: theme.textTheme.titleMedium),
          const SizedBox(height: 8),
          if (_config.rules.isEmpty)
            Text(
              'No rules: every message alerts. Add one to ignore, silence, or '
              'prioritise messages by tag — e.g. ignore "leave", alert "join".',
              style: theme.textTheme.bodySmall,
            ),
          for (var i = 0; i < _config.rules.length; i++)
            Card(
              child: ListTile(
                title: Text(_ruleLabel(_config.rules[i])),
                subtitle: Text(_config.rules[i].effect.name),
                trailing: IconButton(
                  icon: const Icon(Icons.delete_outline),
                  onPressed: () => setState(() {
                    final rules = [..._config.rules]..removeAt(i);
                    _config = AppConfig(
                      relay: _config.relay,
                      topic: _config.topic,
                      serverFilterNone: _config.serverFilterNone,
                      rules: rules,
                    );
                  }),
                ),
              ),
            ),
          const SizedBox(height: 12),
          OutlinedButton.icon(
            onPressed: _config.rules.isEmpty ? null : _save,
            icon: const Icon(Icons.save_outlined),
            label: const Text('Apply rules'),
          ),
        ],
      ),
    );
  }

  static String _ruleLabel(Rule r) {
    final parts = <String>[];
    if (r.tagsAll?.isNotEmpty ?? false) parts.add('all: ${r.tagsAll!.join(", ")}');
    if (r.tagsAny?.isNotEmpty ?? false) parts.add('any: ${r.tagsAny!.join(", ")}');
    if (r.tagsNone?.isNotEmpty ?? false) parts.add('not: ${r.tagsNone!.join(", ")}');
    return parts.isEmpty ? 'every message' : parts.join(' · ');
  }

  @override
  void dispose() {
    _relay.dispose();
    _topic.dispose();
    super.dispose();
  }
}

class RuleEditor extends StatefulWidget {
  const RuleEditor({super.key, required this.onSave});
  final void Function(Rule) onSave;

  @override
  State<RuleEditor> createState() => _RuleEditorState();
}

class _RuleEditorState extends State<RuleEditor> {
  final _any = TextEditingController();
  final _none = TextEditingController();
  Effect _effect = Effect.alert;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.all(16),
    child: Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        const Text('New rule', style: TextStyle(fontSize: 18)),
        const SizedBox(height: 12),
        TextField(
          controller: _any,
          decoration: const InputDecoration(
            labelText: 'Alert if tags include any of',
            hintText: 'join, goal',
            border: OutlineInputBorder(),
          ),
        ),
        const SizedBox(height: 8),
        TextField(
          controller: _none,
          decoration: const InputDecoration(
            labelText: 'Ignore if tags include any of',
            hintText: 'leave',
            border: OutlineInputBorder(),
          ),
        ),
        const SizedBox(height: 12),
        SegmentedButton<Effect>(
          segments: const [
            ButtonSegment(value: Effect.alert, label: Text('Alert')),
            ButtonSegment(value: Effect.silent, label: Text('Silent')),
            ButtonSegment(value: Effect.ignore, label: Text('Ignore')),
          ],
          selected: {_effect},
          onSelectionChanged: (s) => setState(() => _effect = s.first),
        ),
        const SizedBox(height: 16),
        Row(
          mainAxisAlignment: MainAxisAlignment.end,
          children: [
            TextButton(onPressed: () => Navigator.pop(context), child: const Text('Cancel')),
            const SizedBox(width: 8),
            FilledButton(
              onPressed: () {
                final any = _split(_any.text);
                final none = _split(_none.text);
                if (any.isEmpty && none.isEmpty) return;
                widget.onSave(Rule(
                  tagsAny: any.isEmpty ? null : any,
                  tagsNone: none.isEmpty ? null : none,
                  effect: _effect,
                ));
                Navigator.pop(context);
              },
              child: const Text('Add'),
            ),
          ],
        ),
      ],
    ),
  );

  static List<String> _split(String s) =>
      s.split(RegExp(r'[, ]+')).map((t) => t.trim()).where((t) => t.isNotEmpty).toList();

  @override
  void dispose() {
    _any.dispose();
    _none.dispose();
    super.dispose();
  }
}
