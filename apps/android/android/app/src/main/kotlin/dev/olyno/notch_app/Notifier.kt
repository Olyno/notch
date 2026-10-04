package dev.olyno.notch_app

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationChannelGroup
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.media.AudioAttributes
import android.net.Uri
import android.os.Build
import android.provider.Settings
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat

object NotifIds {
    const val SERVICE = 1
    /** Stable per-notification ids derived from seq, offset clear of SERVICE. */
    fun forSeq(seq: Long): Int = (2 + (seq % 100000)).toInt()
}

/**
 * Native notification rendering. All Android-version-specific behaviour is
 * decided here so the Dart service stays platform-agnostic.
 *
 * Android 13+ model:
 *  - One [NotificationChannel] per (topic|channel) — the OS-level switch the
 *    user can tune; we set importance from the message priority.
 *  - `silent` posts to a LOW/BYPASSING-distinct channel with no sound.
 *  - `dedup_key` reuses the notification id -> in-place update, no stacking.
 *  - `timeout_ms` schedules an auto-cancel (cancels still respect it via
 *    the AlarmManager-free Handler: the service owns the timer).
 *  - category maps to NotificationCompat.Category for heads-up ordering /
 *    Car/Wear routing; icon maps to a drawable.
 */
object Notifier {

    private const val GROUP = "dev.olyno.notch_app"

    fun post(ctx: Context, a: Map<String, Any?>) {
        val mgr = NotificationManagerCompat.from(ctx)
        if (!areNotificationsEnabled(ctx)) return

        val topic = a["topic"] as? String ?: "notch"
        val channelKey = a["channel"] as? String ?: topic
        val priority = a["priority"] as? String ?: "def"
        val silent = a["silent"] as? Boolean ?: false
        val seq = (a["seq"] as? Number)?.toLong() ?: 0L
        val title = a["title"] as? String ?: topic
        val body = a["body"] as? String ?: ""
        val category = a["category"] as? String
        val icon = a["icon"] as? String
        val dedupKey = a["dedupKey"] as? String
        val timeoutMs = (a["timeoutMs"] as? Number)?.toLong() ?: 0L
        val url = a["url"] as? String

        val channelId = ensureChannel(ctx, channelKey, silent, priority)

        val id = if (dedupKey != null && dedupKey.isNotEmpty())
            ("nk" + dedupKey).hashCode() and 0x7fffffff
        else
            NotifIds.forSeq(seq)

        val builder = NotificationCompat.Builder(ctx, channelId)
            .setSmallIcon(iconRes(ctx, icon))
            .setContentTitle(title)
            .setContentText(body)
            .setStyle(NotificationCompat.BigTextStyle().bigText(body))
            .setAutoCancel(true)
            .setGroup(GROUP)
            .setContentIntent(tapIntent(ctx, url, id))

        when (priority) {
            "min" -> builder.priority = NotificationCompat.PRIORITY_MIN
            "low" -> builder.priority = NotificationCompat.PRIORITY_LOW
            "high" -> builder.priority = NotificationCompat.PRIORITY_HIGH
            "max" -> builder.priority = NotificationCompat.PRIORITY_MAX
            else -> builder.priority = NotificationCompat.PRIORITY_DEFAULT
        }
        mapCategory(category)?.let { builder.setCategory(it) }
        if (silent) {
            builder.setPriority(NotificationCompat.PRIORITY_LOW)
                .setSound(null)
                .setVibrate(null)
        }
        // heads-up for high/max unless silent
        if (!silent && (priority == "high" || priority == "max")) {
            builder.setFullScreenIntent(tapIntent(ctx, url, id), true)
        }
        if (timeoutMs > 0) {
            builder.setTimeoutAfter(timeoutMs) // API 24+; harmless earlier
        }

        try {
            mgr.notify(id, builder.build())
        } catch (_: SecurityException) {
            // permission revoked mid-flight: drop quietly, socket stays up
        }
    }

    /** One channel per (key, silent) so a silent topic and an alerting one
     *  don't share an importance the user set. */
    private fun ensureChannel(ctx: Context, key: String, silent: Boolean, priority: String): String {
        val id = sanitize("$key${if (silent) "_silent" else ""}")
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val nm = ctx.getSystemService(NotificationManager::class.java)
            nm.createNotificationChannelGroup(
                NotificationChannelGroup(GROUP, "Notch")
            )
            if (nm.getNotificationChannel(id) == null) {
                val importance = when {
                    silent -> NotificationManager.IMPORTANCE_LOW
                    priority == "max" || priority == "high" -> NotificationManager.IMPORTANCE_HIGH
                    priority == "low" || priority == "min" -> NotificationManager.IMPORTANCE_LOW
                    else -> NotificationManager.IMPORTANCE_DEFAULT
                }
                val ch = NotificationChannel(id, key, importance).apply {
                    group = GROUP
                    if (silent) {
                        setSound(null, null)
                        enableVibration(false)
                        lockscreenVisibility = Notification.VISIBILITY_SECRET
                    }
                    description = "notch: $key"
                }
                nm.createNotificationChannel(ch)
            }
        }
        return id
    }

    fun serviceNotification(ctx: Context, connected: Boolean): Notification {
        val nm = ctx.getSystemService(NotificationManager::class.java)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O &&
            nm.getNotificationChannel(CHANNEL_SERVICE) == null
        ) {
            nm.createNotificationChannel(
                NotificationChannel(
                    CHANNEL_SERVICE,
                    "Notch service",
                    NotificationManager.IMPORTANCE_LOW
                ).apply { description = "Notch listener status"; setShowBadge(false) }
            )
        }
        return NotificationCompat.Builder(ctx, CHANNEL_SERVICE)
            .setSmallIcon(R.drawable.notch_small)
            .setContentTitle("Notch")
            .setContentText(if (connected) "Listening" else "Reconnecting…")
            .setOngoing(true)
            .setSilent(true)
            .setPriority(NotificationCompat.PRIORITY_LOW)
            .setForegroundServiceBehavior(NotificationCompat.FOREGROUND_SERVICE_IMMEDIATE)
            .build()
    }

    fun updateServiceNotification(ctx: Context, connected: Boolean) {
        NotificationManagerCompat.from(ctx)
            .notify(NotifIds.SERVICE, serviceNotification(ctx, connected))
    }

    const val CHANNEL_SERVICE = "notch_service"

    private fun openAppIntent(ctx: Context, id: Int): PendingIntent {
        val i = Intent(ctx, MainActivity::class.java).apply {
            flags = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP
        }
        return PendingIntent.getActivity(
            ctx, id, i,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )
    }

    /**
     * Tap action: a publisher `url` opens its deep link — resolved NOW via
     * PackageManager and pinned to the winning package, so the PendingIntent
     * is explicit + immutable (no hijack window, no crash at fire time).
     * http/https with no app registered opens the browser chooser; a scheme
     * nobody handles falls back to opening the Notch app itself.
     */
    private fun tapIntent(ctx: Context, url: String?, id: Int): PendingIntent {
        val view = url?.takeIf { it.isNotBlank() }?.let { u ->
            runCatching { Intent(Intent.ACTION_VIEW, Uri.parse(u)) }.getOrNull()
        } ?: return openAppIntent(ctx, id)
        view.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        val target = ctx.packageManager.resolveActivity(
            view, PackageManager.MATCH_DEFAULT_ONLY
        )?.activityInfo
        return when {
            target != null -> {
                view.setPackage(target.packageName)
                PendingIntent.getActivity(ctx, id, view,
                    PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
            }
            view.data?.scheme == "http" || view.data?.scheme == "https" -> PendingIntent.getActivity(
                ctx, id, Intent.createChooser(view, null)
                    .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
            else -> openAppIntent(ctx, id)
        }
    }

    private fun areNotificationsEnabled(ctx: Context): Boolean {
        if (Build.VERSION.SDK_INT >= 33 &&
            ContextCompat.checkSelfPermission(ctx, android.Manifest.permission.POST_NOTIFICATIONS)
            != PackageManager.PERMISSION_GRANTED
        ) return false
        return NotificationManagerCompat.from(ctx).areNotificationsEnabled()
    }

    private fun mapCategory(c: String?): String? = when (c) {
        "msg" -> Notification.CATEGORY_MESSAGE
        "call" -> Notification.CATEGORY_CALL
        "email" -> Notification.CATEGORY_EMAIL
        "event" -> Notification.CATEGORY_EVENT
        "alarm" -> Notification.CATEGORY_ALARM
        "promo" -> Notification.CATEGORY_PROMO
        "location" -> Notification.CATEGORY_LOCATION_SHARING
        "navigation" -> Notification.CATEGORY_NAVIGATION
        "progress" -> Notification.CATEGORY_PROGRESS
        "social" -> Notification.CATEGORY_SOCIAL
        "error" -> Notification.CATEGORY_ERROR
        "workout" -> Notification.CATEGORY_WORKOUT
        else -> null
    }

    private fun iconRes(ctx: Context, name: String?): Int {
        // Map the logical icon names from notch-core to the app drawables.
        val res = when (name) {
            "info" -> "notch_info"
            "warn" -> "notch_warn"
            "error" -> "notch_error"
            "check" -> "notch_check"
            "message" -> "notch_message"
            "person" -> "notch_person"
            "game" -> "notch_game"
            "timer" -> "notch_timer"
            "battery" -> "notch_battery"
            "network" -> "notch_network"
            "lock" -> "notch_lock"
            else -> null
        }
        val id = if (res != null) ctx.resources.getIdentifier(res, "drawable", ctx.packageName) else 0
        return if (id != 0) id else R.drawable.notch_small
    }

    private fun sanitize(s: String): String =
        s.lowercase().map { if (it.isLetterOrDigit() || it == '_' || it == '-' || it == '.') it else '_' }
            .joinToString("")
}
