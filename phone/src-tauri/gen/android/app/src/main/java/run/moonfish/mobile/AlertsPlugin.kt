package run.moonfish.mobile

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.provider.Settings
import android.webkit.WebView
import androidx.appcompat.app.AppCompatActivity
import androidx.core.content.ContextCompat
import android.content.pm.PackageManager
import app.tauri.PermissionState
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.Permission
import app.tauri.annotation.PermissionCallback
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Channel
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin

private const val NOTIFICATIONS = "notifications"

@InvokeArg
class ListenArgs {
    lateinit var handler: Channel
}

@InvokeArg
class AlertArgs {
    var chatId: String = ""
    var desktopId: String = ""
    /** `approval`, `plan`, `question`, `done` or `failed`. */
    var kind: String = "approval"
    var title: String = ""
    var body: String = ""
    var detail: String? = null
    /** An approval Allow and Deny answer. */
    var requestId: String? = null
    var loud: Boolean = true
}

@InvokeArg
class ChatArgs {
    var chatId: String = ""
}

@InvokeArg
class WatchArgs {
    var title: String = ""
    var text: String = ""
    var lost: Boolean = false
}

@InvokeArg
class NoteArgs {
    var text: String? = null
}

/** The Rust side's way to Alerts (see Alerts.kt): src/alerts.rs calls these. */
@TauriPlugin(permissions = [Permission(strings = [Manifest.permission.POST_NOTIFICATIONS], alias = NOTIFICATIONS)])
class AlertsPlugin(private val activity: Activity) : Plugin(activity) {
    override fun load(webView: WebView) {
        Alerts.clearStale(activity)
        // Opened by tapping a notification: the app reads it as it starts (`takeOpen`).
        Alerts.opened(activity.intent)
    }

    override fun onNewIntent(intent: Intent) {
        if (Alerts.opened(intent)) Alerts.listener?.send(JSObject().put("kind", "open"))
    }

    override fun onResume(activity: AppCompatActivity) {
        Alerts.resume(activity.applicationContext)
    }

    /** `granted`, `prompt` (Android can still ask) or `denied` (only Android's settings can
     *  change it). */
    private fun state(): String = when {
        Alerts.canNotify(activity) -> "granted"
        Build.VERSION.SDK_INT >= 33 &&
            ContextCompat.checkSelfPermission(activity, Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED &&
            getPermissionState(NOTIFICATIONS) != PermissionState.DENIED -> "prompt"
        else -> "denied"
    }

    @Command
    fun listen(invoke: Invoke) {
        Alerts.listener = invoke.parseArgs(ListenArgs::class.java).handler
        invoke.resolve()
    }

    @Command
    fun permission(invoke: Invoke) {
        invoke.resolve(JSObject().put("state", state()))
    }

    /** Asks for notifications (Android 13+), only when alerts are turned on. Always asks
     *  Android when they aren't allowed: a dialog dismissed earlier can still come up, and
     *  Android itself answers at once when it won't ask again. */
    @Command
    fun requestPermission(invoke: Invoke) {
        val allowed = Build.VERSION.SDK_INT < 33 ||
            ContextCompat.checkSelfPermission(activity, Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED
        if (allowed) {
            invoke.resolve(JSObject().put("state", state()))
        } else {
            requestPermissionForAlias(NOTIFICATIONS, invoke, "permissionDone")
        }
    }

    @PermissionCallback
    fun permissionDone(invoke: Invoke) {
        invoke.resolve(JSObject().put("state", state()))
    }

    /** Android's notification settings for Moonfish (after "Don't allow"). */
    @Command
    fun openSettings(invoke: Invoke) {
        val intent = if (Build.VERSION.SDK_INT >= 26) {
            Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS).putExtra(Settings.EXTRA_APP_PACKAGE, activity.packageName)
        } else {
            Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.parse("package:" + activity.packageName))
        }
        try {
            activity.startActivity(intent)
            invoke.resolve()
        } catch (e: Exception) {
            invoke.reject(e.message ?: "Couldn't open Android's settings")
        }
    }

    /** The chat a notification tap asked to open, once. */
    @Command
    fun takeOpen(invoke: Invoke) {
        val open = Alerts.pendingOpen
        Alerts.pendingOpen = null
        invoke.resolve(open ?: JSObject())
    }

    @Command
    fun alert(invoke: Invoke) {
        Alerts.alert(activity.applicationContext, invoke.parseArgs(AlertArgs::class.java))
        invoke.resolve()
    }

    @Command
    fun clear(invoke: Invoke) {
        Alerts.clear(activity.applicationContext, invoke.parseArgs(ChatArgs::class.java).chatId)
        invoke.resolve()
    }

    @Command
    fun watch(invoke: Invoke) {
        val args = invoke.parseArgs(WatchArgs::class.java)
        Alerts.watch(activity.applicationContext, Alerts.Watch(args.title, args.text, args.lost))
        invoke.resolve()
    }

    @Command
    fun release(invoke: Invoke) {
        Alerts.release()
        invoke.resolve()
    }

    @Command
    fun note(invoke: Invoke) {
        Alerts.note(activity.applicationContext, invoke.parseArgs(NoteArgs::class.java).text)
        invoke.resolve()
    }
}
