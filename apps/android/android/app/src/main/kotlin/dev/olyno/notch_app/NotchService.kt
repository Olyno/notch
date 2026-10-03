package dev.olyno.notch_app

import android.app.Service
import android.content.Context
import android.content.Intent
import android.os.IBinder
import io.flutter.FlutterInjector
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.embedding.engine.dart.DartExecutor
import io.flutter.plugin.common.MethodChannel

/**
 * Foreground service that runs the realtime listener.
 *
 * Owns a background [FlutterEngine] executing `serviceMain()` (lib/
 * service_entry.dart): that Dart isolate holds the NotchClient socket,
 * applies rules, and posts finished notifications back through the
 * `notch.service` channel.
 */
class NotchService : Service() {

    private var engine: FlutterEngine? = null

    override fun onCreate() {
        super.onCreate()
        startForeground(NotifIds.SERVICE, Notifier.serviceNotification(this, false))
        isRunning = true

        val e = FlutterEngine(applicationContext)
        // The channel MUST exist before the entrypoint runs: serviceMain()
        // calls bootstrap() as its first act.
        MethodChannel(e.dartExecutor.binaryMessenger, CHANNEL).setMethodCallHandler { call, result ->
            when (call.method) {
                "bootstrap" -> result.success(applicationContext.filesDir.absolutePath)

                "post" -> {
                    @Suppress("UNCHECKED_CAST")
                    val args = call.arguments as? Map<String, Any?>
                    if (args != null) Notifier.post(this, args)
                    result.success(null)
                }

                "status" -> {
                    val connected = call.argument<Boolean>("connected") ?: false
                    Notifier.updateServiceNotification(this, connected)
                    result.success(null)
                }

                else -> result.notImplemented()
            }
        }
        e.dartExecutor.executeDartEntrypoint(
            DartExecutor.DartEntrypoint(
                FlutterInjector.instance().flutterLoader().findAppBundlePath(),
                // 2-arg ctor would read "serviceMain" as a *library* with
                // function "main" — use the explicit library + function form.
                "package:notch_app/service_entry.dart",
                "serviceMain",
            )
        )
        engine = e
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int = START_STICKY

    override fun onDestroy() {
        isRunning = false
        engine?.destroy()
        engine = null
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    companion object {
        const val CHANNEL = "notch.service"

        @Volatile
        var isRunning = false
            private set

        fun start(context: Context) =
            context.startForegroundService(Intent(context, NotchService::class.java))

        fun stop(context: Context) =
            context.stopService(Intent(context, NotchService::class.java))
    }
}
