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

    /**
     * Arranca o serviço. No Android 12+ só é permitido com a app visível, a partir de `BOOT_COMPLETED`, ou
     * quando acorda por uma mensagem FCM de prioridade alta; de um receptor em segundo plano o sistema recusa
     * (`ForegroundServiceStartNotAllowedException`). Devolve `false` nesse caso, sem deitar o processo abaixo:
     * fica «ligado» e o próximo momento permitido (abrir a app, reiniciar) arranca-o.
     */
    fun start(context: Context): Boolean {
        Store.setEnabled(context, true)
        return try {
            context.startForegroundService(Intent(context, PushService::class.java))
            true
        } catch (e: IllegalStateException) { // inclui ForegroundServiceStartNotAllowedException
            false
        }
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
