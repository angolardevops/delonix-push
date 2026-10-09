package ao.ngolacloud.push.android

import android.content.Context

/** Configuração em armazenamento privado da app (não é copiada para cópias de segurança: ver o manifesto da app). */
internal object Store {
    private fun prefs(c: Context) = c.applicationContext.getSharedPreferences("delonix_push", Context.MODE_PRIVATE)

    fun save(c: Context, url: String, secret: String) {
        prefs(c).edit().putString("url", url).putString("secret", secret).apply()
    }

    fun load(c: Context): Pair<String, String>? {
        val p = prefs(c)
        val url = p.getString("url", null) ?: return null
        val secret = p.getString("secret", null) ?: return null
        return url to secret
    }

    fun setEnabled(c: Context, on: Boolean) { prefs(c).edit().putBoolean("enabled", on).apply() }
    fun enabled(c: Context) = prefs(c).getBoolean("enabled", false)
    fun clear(c: Context) { prefs(c).edit().clear().apply() }
}
