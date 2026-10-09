package ao.ngolacloud.push.android

import android.content.Context
import android.content.Intent
import ao.ngolacloud.push.PushMessage

/** Quem trata as mensagens. Define-se em `Application.onCreate`: o serviço pode nascer sem a Activity. */
fun interface PushHandler {
    fun onMessage(context: Context, message: PushMessage)
}

/**
 * Porta de entrada do SDK.
 *
 * ```kotlin
 * // Application.onCreate
 * DelonixPush.handler = PushHandler { ctx, m -> /* mostrar a chamada, acordar o SIP… */ }
 * // depois de o Meet devolver {url, device_id, device_secret}
 * DelonixPush.configure(this, url, deviceSecret); DelonixPush.start(this)
 * ```
 * Num Android com Google Play Services pode juntar-se o FCM: o `FirebaseMessagingService` da app chama
 * [wake] e o serviço sobe, liga-se e recebe a mensagem com *ack*.
 */
object DelonixPush {
    @Volatile var handler: PushHandler? = null

    /** Texto da notificação permanente (obrigatória num serviço em primeiro plano). */
    @Volatile var notificationTitle: String = "Ligação de chamadas activa"

    fun configure(context: Context, baseUrl: String, deviceSecret: String) {
        Store.save(context, baseUrl, deviceSecret)
    }

    fun isConfigured(context: Context) = Store.load(context) != null

    fun start(context: Context) {
        Store.setEnabled(context, true)
        context.startForegroundService(Intent(context, PushService::class.java))
    }

    fun stop(context: Context) {
        Store.setEnabled(context, false)
        context.stopService(Intent(context, PushService::class.java))
    }

    /** Para o FCM (ou um alarme) acordar o serviço quando a app está morta. */
    fun wake(context: Context) {
        if (Store.load(context) != null && Store.enabled(context)) start(context)
    }

    /** Esquece o segredo (terminar a sessão, aparelho revogado). */
    fun clear(context: Context) {
        stop(context)
        Store.clear(context)
    }
}
