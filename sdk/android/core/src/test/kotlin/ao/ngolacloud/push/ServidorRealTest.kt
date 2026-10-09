package ao.ngolacloud.push

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import java.net.ServerSocket
import java.net.URI
import java.net.http.HttpClient
import java.net.http.HttpRequest
import java.net.http.HttpResponse
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import kotlin.test.AfterTest
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertTrue

/**
 * O cliente contra o servidor REAL (o binário Rust). Só corre com `DELONIX_PUSH_BIN` e `DELONIX_PUSH_DB`
 * (ver `e2e.sh`): os testes com MockWebServer provam o cliente contra um papel, este prova-o contra o serviço.
 */
class ServidorRealTest {
    private val bin = System.getenv("DELONIX_PUSH_BIN")
    private val db = System.getenv("DELONIX_PUSH_DB")
    private var proc: Process? = null
    private val http = HttpClient.newHttpClient()
    private val json = Json { ignoreUnknownKeys = true }
    private lateinit var base: String

    private fun arranca() {
        val port = ServerSocket(0).use { it.localPort }
        base = "http://127.0.0.1:$port"
        val pb = ProcessBuilder(bin).redirectErrorStream(true).redirectOutput(ProcessBuilder.Redirect.DISCARD)
        pb.environment().putAll(mapOf("DATABASE_URL" to db, "PUSH_BIND" to "127.0.0.1:$port", "PUSH_ADMIN_TOKEN" to "admin-e2e"))
        proc = pb.start()
        val t0 = System.currentTimeMillis()
        while (System.currentTimeMillis() - t0 < 20_000) {
            if (runCatching { post("/healthz", "", null, get = true).first == 200 }.getOrDefault(false)) return
            Thread.sleep(100)
        }
        error("o servidor não arrancou")
    }

    @AfterTest fun fim() { proc?.destroy(); proc?.waitFor(5, TimeUnit.SECONDS) }

    private fun post(path: String, body: String, auth: String?, get: Boolean = false, admin: Boolean = false, method: String? = null): Pair<Int, String> {
        val b = HttpRequest.newBuilder(URI("$base$path")).header("content-type", "application/json")
        auth?.let { b.header("authorization", "Bearer $it") }
        if (admin) b.header("x-admin-token", "admin-e2e")
        val rq = when {
            get -> b.GET()
            method == "DELETE" -> b.DELETE()
            else -> b.POST(HttpRequest.BodyPublishers.ofString(body))
        }.build()
        val r = http.send(rq, HttpResponse.BodyHandlers.ofString())
        return r.statusCode() to r.body()
    }

    private fun campo(s: String, k: String) = json.parseToJsonElement(s).jsonObject[k]!!.jsonPrimitive.content

    private class Escuta : PushListener {
        val q = LinkedBlockingQueue<PushMessage>()
        @Volatile var authFailed = false
        override fun onMessage(message: PushMessage) { q.add(message) }
        override fun onAuthFailed() { authFailed = true }
    }

    @Test fun entregaComAckRealEFilaOffline() {
        if (bin == null || db == null) return
        arranca()
        val (_, proj) = post("/admin/v1/projects", """{"name":"e2e"}""", null, admin = true)
        val key = campo(proj, "server_key")
        val (_, dev) = post("/v1/devices", """{"platform":"android"}""", key)
        val id = campo(dev, "device_id"); val secret = campo(dev, "device_secret")

        // Offline: a mensagem espera na fila do servidor.
        val (st, env) = post("/v1/messages", """{"device_id":"$id","payload":{"tipo":"chamada","n":7},"priority":"high"}""", key)
        assertEquals(202, st)
        val msgId = json.parseToJsonElement(env).jsonObject["message_ids"]!!.let { (it as kotlinx.serialization.json.JsonArray)[0].jsonPrimitive.content }
        assertEquals("queued", campo(post("/v1/messages/$msgId", "", key, get = true).second, "state"))

        val l = Escuta()
        val c = PushClient(PushConfig(base, secret, minBackoffMs = 100, maxBackoffMs = 500), l)
        c.start()
        val m = assertNotNull(l.q.poll(5, TimeUnit.SECONDS), "a mensagem em fila chegou ao ligar")
        assertEquals(msgId, m.id)
        assertEquals("chamada", m.payload.jsonObject["tipo"]!!.jsonPrimitive.content)
        assertTrue(m.highPriority)
        waitState(key, msgId, "delivered")

        // Ligado: a mensagem chega na hora.
        val (_, e2) = post("/v1/messages", """{"device_id":"$id","payload":"ligado"}""", key)
        assertEquals("ligado", assertNotNull(l.q.poll(5, TimeUnit.SECONDS)).payload.jsonPrimitive.content)
        assertTrue(e2.contains("message_ids"))
        c.shutdown()
    }

    @Test fun aparelhoRevogadoNaoVoltaALigarSePara() {
        if (bin == null || db == null) return
        arranca()
        val key = campo(post("/admin/v1/projects", """{"name":"e2e2"}""", null, admin = true).second, "server_key")
        val (_, dev) = post("/v1/devices", """{"platform":"android"}""", key)
        val id = campo(dev, "device_id"); val secret = campo(dev, "device_secret")
        assertEquals(204, post("/v1/devices/$id", "", key, method = "DELETE").first)
        val l = Escuta(); val c = PushClient(PushConfig(base, secret, minBackoffMs = 100), l)
        c.start()
        val t0 = System.currentTimeMillis()
        while (!l.authFailed && System.currentTimeMillis() - t0 < 5000) Thread.sleep(20)
        assertTrue(l.authFailed, "o servidor real recusa o segredo revogado e o cliente pára")
        assertEquals(PushClient.State.AuthFailed, c.state)
        c.shutdown()
    }

    private fun waitState(key: String, id: String, want: String) {
        val t0 = System.currentTimeMillis()
        var s = ""
        while (System.currentTimeMillis() - t0 < 5000) {
            s = campo(post("/v1/messages/$id", "", key, get = true).second, "state")
            if (s == want) return
            Thread.sleep(50)
        }
        error("estado $s em vez de $want")
    }
}
