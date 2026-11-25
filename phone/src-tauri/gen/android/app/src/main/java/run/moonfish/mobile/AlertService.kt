package run.moonfish.mobile

import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder

/**
 * Keeps Moonfish running in the background while a chat on the desktop is busy or waits on you, so
 * its connection stays open and Alerts can tell you. Nothing else happens in it: the app's
 * Rust side does the work and asks for this (Alerts.watch / Alerts.release).
 *
 * A "special use" foreground service: the standard types don't fit a link to your own desktop
 * that's held only while its chats run (dataSync stops after 6 hours a day on Android 15+;
 * connectedDevice is for accessories). Moonfish is sideloaded, so no store review applies.
 */
class AlertService : Service() {
    companion object {
        /** Set while it runs in the foreground. */
        @Volatile var instance: AlertService? = null
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        // A service started this way must go to the foreground, even if it's no longer wanted.
        val w = Alerts.wanted ?: Alerts.Watch("Following chats", "", false)
        try {
            val n = Alerts.watchNotification(this, w)
            if (Build.VERSION.SDK_INT >= 34) {
                startForeground(Alerts.WATCH_ID, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE)
            } else {
                startForeground(Alerts.WATCH_ID, n)
            }
        } catch (e: Exception) {
            stopSelf()
            return START_NOT_STICKY
        }
        instance = this
        if (Alerts.wanted == null) finish()
        // Not brought back by Android on its own: it only means something while the app runs.
        return START_NOT_STICKY
    }

    /** Stops, unless it's wanted again by now. Runs on the main thread. */
    fun finish() {
        if (Alerts.wanted != null) return
        instance = null
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    override fun onDestroy() {
        if (instance === this) instance = null
        super.onDestroy()
    }
}
