package ao.ngolacloud.push.android

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/** Depois de reiniciar o telemóvel ou actualizar a app, volta a ligar se estava ligado. */
class BootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        DelonixPush.wake(context)
    }
}
