package dev.olyno.notch_app

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import java.io.File

/**
 * Resume listening after a reboot — but only if the user ever configured
 * the app (config.json exists). No config = no service = no battery drain.
 */
class BootReceiver : BroadcastReceiver() {

    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != Intent.ACTION_BOOT_COMPLETED) return
        if (!File(context.filesDir, "config.json").exists()) return
        try {
            NotchService.start(context)
        } catch (_: Exception) {
            // background start restrictions during boot: next unlock/boot retries
        }
    }
}
