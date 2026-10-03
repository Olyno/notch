package dev.olyno.notch_app

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.PowerManager
import android.provider.Settings
import androidx.core.app.ActivityCompat
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel
import java.io.File

/**
 * UI bridge (`notch.ui` channel): the settings screen commands the service
 * and asks the user for the two Android 13+ realities that keep a
 * self-hosted listener alive: the notification permission and a battery
 * optimization exemption.
 */
class MainActivity : FlutterActivity() {

    private var pendingNotif: MethodChannel.Result? = null

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        // Headless/kiosk + device tests: `am start --es notch.autostart 1`
        // begins listening without a tap (config must already exist).
        if (intent?.getStringExtra("notch.autostart") == "1" &&
            File(applicationContext.filesDir, "config.json").exists()
        ) {
            NotchService.start(this)
        }
        MethodChannel(flutterEngine.dartExecutor.binaryMessenger, "notch.ui")
            .setMethodCallHandler { call, result ->
                when (call.method) {
                    "start" -> {
                        NotchService.start(this)
                        result.success(true)
                    }

                    "stop" -> {
                        NotchService.stop(this)
                        result.success(true)
                    }

                    "running" -> result.success(NotchService.isRunning)
                    "filesDir" -> result.success(applicationContext.filesDir.absolutePath)
                    "requestNotifications" -> requestNotifications(result)
                    "requestBatteryExemption" -> requestBatteryExemption(result)
                    else -> result.notImplemented()
                }
            }
    }

    private fun requestNotifications(result: MethodChannel.Result) {
        if (Build.VERSION.SDK_INT < 33 ||
            checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) ==
            PackageManager.PERMISSION_GRANTED
        ) {
            result.success(true)
            return
        }
        pendingNotif = result
        ActivityCompat.requestPermissions(
            this, arrayOf(Manifest.permission.POST_NOTIFICATIONS), REQ_NOTIF
        )
    }

    override fun onRequestPermissionsResult(
        requestCode: Int,
        permissions: Array<out String>,
        grantResults: IntArray,
    ) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (requestCode == REQ_NOTIF) {
            val granted =
                grantResults.isNotEmpty() && grantResults[0] == PackageManager.PERMISSION_GRANTED
            pendingNotif?.success(granted)
            pendingNotif = null
        }
    }

    private fun requestBatteryExemption(result: MethodChannel.Result) {
        // Best effort: a user who declines still gets notifications, just
        // less reliably on aggressive OEM battery managers.
        val pm = getSystemService(PowerManager::class.java)
        if (pm != null && pm.isIgnoringBatteryOptimizations(packageName)) {
            result.success(true)
            return
        }
        try {
            startActivity(
                Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS).apply {
                    data = Uri.parse("package:$packageName")
                }
            )
            result.success(true)
        } catch (_: Exception) {
            result.success(false)
        }
    }

    companion object {
        private const val REQ_NOTIF = 42
    }
}
