package run.moonfish.mobile

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.ProcessLifecycleOwner
import app.tauri.plugin.Channel
import app.tauri.plugin.JSObject

/**
 * Alerts on this phone (Settings → Alerts, off until turned on): the notifications the Rust side
 * (src/alerts.rs) asks for, one per chat, the Allow and Deny on an approval's, and the service
 * that keeps the desktop's connection open while its chats run (AlertService). Rust decides what
 * to show; this only shows it.
 */
object Alerts {
    private const val NEEDS = "moonfish.needs"
    private const val DONE = "moonfish.done"
    private const val WATCH = "moonfish.watch"
    private const val CHAT_TAG = "chat:"
    private const val NOTE_TAG = "note"
    private const val ALERT_ID = 2
    const val WATCH_ID = 1

    const val ACTION_OPEN = "run.moonfish.mobile.OPEN_CHAT"
    const val ACTION_ANSWER = "run.moonfish.mobile.ANSWER"
    const val EXTRA_CHAT = "run.moonfish.mobile.chatId"
    const val EXTRA_DESKTOP = "run.moonfish.mobile.desktopId"
    const val EXTRA_REQUEST = "run.moonfish.mobile.requestId"
    const val EXTRA_DECISION = "run.moonfish.mobile.decision"
    const val EXTRA_TITLE = "run.moonfish.mobile.title"

    /** What the kept connection's ongoing notification says. */
    data class Watch(val title: String, val text: String, val lost: Boolean)

    /** Where Allow, Deny and a tap that opened a chat go: the Rust side, while the app runs. */
    @Volatile var listener: Channel? = null
    /** The chat a notification asked to open, until the app reads it. */
    @Volatile var pendingOpen: JSObject? = null
    /** Set while Rust wants the connection kept; the service shows it. */
    @Volatile var wanted: Watch? = null

    private fun manager(ctx: Context) = ctx.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager

    /** Notifications can show: allowed (Android 13+ asks) and not turned off for Moonfish. */
    fun canNotify(ctx: Context): Boolean {
        if (Build.VERSION.SDK_INT >= 33 &&
            ContextCompat.checkSelfPermission(ctx, Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) return false
        return NotificationManagerCompat.from(ctx).areNotificationsEnabled()
    }

    private fun visible(): Boolean =
        ProcessLifecycleOwner.get().lifecycle.currentState.isAtLeast(Lifecycle.State.STARTED)

    private fun channels(ctx: Context) {
        if (Build.VERSION.SDK_INT < 26) return
        val m = manager(ctx)
        m.createNotificationChannel(NotificationChannel(NEEDS, "Chats that need you", NotificationManager.IMPORTANCE_HIGH).apply {
            description = "An approval, a question or a plan to review"
        })
        m.createNotificationChannel(NotificationChannel(DONE, "Finished chats", NotificationManager.IMPORTANCE_DEFAULT).apply {
            description = "A chat finished its turn. Enable in Settings, Alerts, Also when a chat is done."
        })
        m.createNotificationChannel(NotificationChannel(WATCH, "Following chats", NotificationManager.IMPORTANCE_LOW).apply {
            description = "Shown while Moonfish keeps the desktop's connection open, and when it's lost"
            setShowBadge(false)
        })
    }

    private fun code(vararg parts: String) = parts.joinToString("|").hashCode()

    private fun immutable() = PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE

    /** Opens the app (and, with a chat, that chat). */
    private fun openIntent(ctx: Context, chatId: String?, desktopId: String?): PendingIntent {
        val intent = Intent(ctx, MainActivity::class.java).apply {
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP)
            if (chatId != null) {
                action = ACTION_OPEN
                putExtra(EXTRA_CHAT, chatId)
                putExtra(EXTRA_DESKTOP, desktopId ?: "")
            }
        }
        return PendingIntent.getActivity(ctx, code("open", chatId ?: ""), intent, immutable())
    }

    private fun answerAction(ctx: Context, a: AlertArgs, decision: String, label: String): NotificationCompat.Action {
        val intent = Intent(ctx, AlertReceiver::class.java).apply {
            action = ACTION_ANSWER
            putExtra(EXTRA_CHAT, a.chatId)
            putExtra(EXTRA_DESKTOP, a.desktopId)
            putExtra(EXTRA_REQUEST, a.requestId)
            putExtra(EXTRA_DECISION, decision)
            putExtra(EXTRA_TITLE, a.title)
        }
        // One per request, not per chat: a tap still waiting on the unlock keeps the request it
        // was shown with, even if a newer one replaces the notification meanwhile.
        val pending = PendingIntent.getBroadcast(ctx, code(decision, a.chatId, a.requestId ?: ""), intent, immutable())
        // Allowing a command on the desktop needs the phone unlocked.
        return NotificationCompat.Action.Builder(0, label, pending).setAuthenticationRequired(true).build()
    }

    private fun builder(ctx: Context, channel: String, title: String, text: String) =
        NotificationCompat.Builder(ctx, channel)
            .setSmallIcon(R.drawable.ic_stat_moonfish)
            .setContentTitle(title)
            .setContentText(text)

    /** Shows (or replaces) a chat's notification. */
    fun alert(ctx: Context, a: AlertArgs) {
        val tag = CHAT_TAG + a.chatId
        if (!canNotify(ctx)) return
        if (a.loud && visible()) {
            manager(ctx).cancel(tag, ALERT_ID)
            return
        }
        channels(ctx)
        val needsYou = a.kind != "done" && a.kind != "failed"
        val channel = if (needsYou) NEEDS else DONE
        val expanded = if (a.detail.isNullOrBlank()) a.body else "${a.body}\n\n${a.detail}"
        // The lock screen says that something needs you, not what.
        val onLockScreen = builder(ctx, channel, a.title, "Unlock to see it").build()
        val n = builder(ctx, channel, a.title, a.body)
            .setStyle(NotificationCompat.BigTextStyle().bigText(expanded))
            .setContentIntent(openIntent(ctx, a.chatId, a.desktopId))
            .setAutoCancel(true)
            .setOnlyAlertOnce(!a.loud)
            .setPriority(if (needsYou) NotificationCompat.PRIORITY_HIGH else NotificationCompat.PRIORITY_DEFAULT)
            .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
            .setPublicVersion(onLockScreen)
        // Allow and Deny only where Android makes them wait for an unlock (12+): on older phones
        // anyone holding the locked phone could tap them. There, the tap opens the chat.
        val request = a.requestId
        if (!request.isNullOrEmpty() && Build.VERSION.SDK_INT >= 31) {
            n.addAction(answerAction(ctx, a, "allow", "Allow"))
            n.addAction(answerAction(ctx, a, "deny", "Deny"))
        }
        manager(ctx).notify(tag, ALERT_ID, n.build())
    }

    fun clear(ctx: Context, chatId: String) {
        manager(ctx).cancel(CHAT_TAG + chatId, ALERT_ID)
    }

    /** An answer tapped on a notification while Moonfish wasn't running: it can't go from here. */
    fun cannotAnswer(ctx: Context, chatId: String, desktopId: String, title: String) {
        if (!canNotify(ctx)) return
        channels(ctx)
        val n = builder(ctx, NEEDS, title, "Open Moonfish to answer. The app wasn't running, so your answer wasn't sent.")
            .setContentIntent(openIntent(ctx, chatId, desktopId))
            .setAutoCancel(true)
            .setOnlyAlertOnce(true)
            .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
            .build()
        manager(ctx).notify(CHAT_TAG + chatId, ALERT_ID, n)
    }

    /** A quiet note (the phone stopped following the desktop), or none. */
    fun note(ctx: Context, text: String?) {
        if (text == null) {
            manager(ctx).cancel(NOTE_TAG, ALERT_ID)
            return
        }
        if (!canNotify(ctx)) return
        channels(ctx)
        val n = builder(ctx, WATCH, "Not following chats", text)
            .setStyle(NotificationCompat.BigTextStyle().bigText(text))
            .setContentIntent(openIntent(ctx, null, null))
            .setAutoCancel(true)
            .setSilent(true)
            .build()
        manager(ctx).notify(NOTE_TAG, ALERT_ID, n)
    }

    /** Notifications from an earlier run of the app: nothing follows them now. */
    fun clearStale(ctx: Context) {
        val m = manager(ctx)
        for (n in m.activeNotifications) {
            val tag = n.tag ?: continue
            if (tag.startsWith(CHAT_TAG) || tag == NOTE_TAG) m.cancel(tag, n.id)
        }
    }

    fun watchNotification(ctx: Context, w: Watch): Notification {
        channels(ctx)
        return builder(ctx, WATCH, w.title, w.text)
            .setContentIntent(openIntent(ctx, null, null))
            .setOngoing(true)
            .setSilent(true)
            .setCategory(NotificationCompat.CATEGORY_SERVICE)
            .setPriority(NotificationCompat.PRIORITY_LOW)
            .setForegroundServiceBehavior(NotificationCompat.FOREGROUND_SERVICE_IMMEDIATE)
            .build()
    }

    /** Keep the connection (the service starts, or says `w` if it runs). */
    fun watch(ctx: Context, w: Watch) {
        wanted = w
        // With notifications off nothing would show: don't keep the phone awake for it.
        if (!canNotify(ctx)) return
        if (AlertService.instance != null) {
            manager(ctx).notify(WATCH_ID, watchNotification(ctx, w))
        } else {
            start(ctx)
        }
    }

    private fun start(ctx: Context) {
        try {
            ContextCompat.startForegroundService(ctx, Intent(ctx, AlertService::class.java))
        } catch (e: IllegalStateException) {
            // Android doesn't start one from the background: it starts when Moonfish is opened.
        } catch (e: SecurityException) {
        }
    }

    /** The app is in front again: start the service if it's wanted and couldn't start before. */
    fun resume(ctx: Context) {
        if (wanted != null && AlertService.instance == null && canNotify(ctx)) start(ctx)
    }

    /** Let the connection go: the service stops. */
    fun release() {
        wanted = null
        val service = AlertService.instance ?: return
        Handler(Looper.getMainLooper()).post { service.finish() }
    }

    /** A notification tap that should open a chat: kept for the app to read (`takeOpen`). */
    fun opened(intent: Intent?): Boolean {
        if (intent == null || intent.action != ACTION_OPEN) return false
        // Reopened from recent apps: the tap that first started it is old news.
        if ((intent.flags and Intent.FLAG_ACTIVITY_LAUNCHED_FROM_HISTORY) != 0) return false
        val chat = intent.getStringExtra(EXTRA_CHAT) ?: return false
        pendingOpen = JSObject().put("chatId", chat).put("desktopId", intent.getStringExtra(EXTRA_DESKTOP) ?: "")
        // Read once: the same intent coming back (the activity made again) isn't a new tap.
        intent.action = null
        return true
    }
}
