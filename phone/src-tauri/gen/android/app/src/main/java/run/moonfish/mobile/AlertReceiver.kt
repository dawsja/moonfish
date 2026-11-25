package run.moonfish.mobile

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import app.tauri.plugin.JSObject

/** Allow or Deny on an approval's notification: the answer goes to the desktop through the
 *  app's Rust side (src/alerts.rs), which updates the notification. */
class AlertReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != Alerts.ACTION_ANSWER) return
        val chat = intent.getStringExtra(Alerts.EXTRA_CHAT) ?: return
        val request = intent.getStringExtra(Alerts.EXTRA_REQUEST) ?: return
        val decision = intent.getStringExtra(Alerts.EXTRA_DECISION) ?: return
        val desktop = intent.getStringExtra(Alerts.EXTRA_DESKTOP) ?: ""
        val listener = Alerts.listener
        if (listener == null) {
            Alerts.cannotAnswer(context, chat, desktop, intent.getStringExtra(Alerts.EXTRA_TITLE) ?: "Approval needed")
            return
        }
        listener.send(
            JSObject()
                .put("kind", "answer")
                .put("chatId", chat)
                .put("desktopId", desktop)
                .put("requestId", request)
                .put("decision", decision),
        )
    }
}
