package ao.ngolacloud.push.android

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.IBinder
import ao.ngolacloud.push.PushClient
import ao.ngolacloud.push.PushConfig
import ao.ngolacloud.push.PushListener
import ao.ngolacloud.push.PushMessage

/** Segura a ligação ao delonix-push enquanto o serviço vive. Reinicia-se sozinho se o sistema o matar. */
class PushService : Service() {
    private var client: PushClient? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        startForeground(NOTIFICATION_ID, notification(), ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE)
        val cfg = Store.load(this)
        if (cfg == null) {
            stopSelf()
            return START_NOT_STICKY
        }
        if (client == null) {
            val ctx = applicationContext
            client = PushClient(PushConfig(cfg.first, cfg.second), object : PushListener {
                override fun onMessage(message: PushMessage) {
                    DelonixPush.handler?.onMessage(ctx, message)
                        ?: error("sem PushHandler: defina DelonixPush.handler em Application.onCreate")
                }

                override fun onAuthFailed() {
                    // Aparelho revogado: deixa de tentar e de mostrar a notificação.
                    Store.setEnabled(ctx, false)
                    stopSelf()
                }
            }).also { it.start() }
        }
        return START_STICKY
    }

    override fun onDestroy() {
        client?.shutdown()
        client = null
        super.onDestroy()
    }

    private fun notification(): Notification {
        val nm = getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(
            NotificationChannel(CHANNEL, "Ligação de chamadas", NotificationManager.IMPORTANCE_MIN),
        )
        return Notification.Builder(this, CHANNEL)
            .setContentTitle(DelonixPush.notificationTitle)
            .setSmallIcon(android.R.drawable.stat_sys_phone_call)
            .setOngoing(true)
            .build()
    }

    private companion object {
        const val CHANNEL = "delonix_push_ligacao"
        const val NOTIFICATION_ID = 7101
    }
}
