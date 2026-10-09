package ao.ngolacloud.push

import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import kotlin.test.AfterTest
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

/** O servidor de papel: guarda o que a app lhe envia e deixa o teste empurrar mensagens. */
private class Papel : WebSocketListener() {
    val recebido = LinkedBlockingQueue<String>()
    @Volatile var ws: WebSocket? = null
    override fun onOpen(webSocket: WebSocket, response: Response) { ws = webSocket }
    override fun onMessage(webSocket: WebSocket, text: String) { recebido.add(text) }
    fun empurra(id: String, payload: String = """{"k":1}""") {
        ws!!.send("""{"type":"message","id":"$id","payload":$payload,"priority":"high","collapse_key":null,"expires_at":"2030-01-01T00:00:00Z"}""")
    }
    fun ack(): String? = generateSequence { recebido.poll(3, TimeUnit.SECONDS) }.firstOrNull { it.contains("\"ack\"") }
}

class PushClientTest {
    private val server = MockWebServer().apply { start() }
    private val clients = mutableListOf<PushClient>()

    @AfterTest fun fim() {
        clients.forEach { it.shutdown() }
        Thread.sleep(300)
        runCatching { server.shutdown() }
    }

    private fun cliente(l: PushListener, seen: SeenStore = MemorySeenStore(), min: Long = 50): PushClient {
        val cfg = PushConfig(server.url("/").toString().trimEnd('/'), "dpd_segredo", minBackoffMs = min, maxBackoffMs = 400, heartbeatMs = 60_000)
        return PushClient(cfg, l, seen).also { clients += it }
    }

    private class Escuta(val falhaNa1: Boolean = false) : PushListener {
        val mensagens = LinkedBlockingQueue<PushMessage>()
        val estados = CopyOnWriteArrayList<PushClient.State>()
        @Volatile var auth = false
        var chamadas = 0
        override fun onMessage(message: PushMessage) {
            chamadas++
            if (falhaNa1 && chamadas == 1) error("falhei")
            mensagens.add(message)
        }
        override fun onStateChanged(state: PushClient.State) { estados += state }
        override fun onAuthFailed() { auth = true }
    }

    private fun ligar(p: Papel) = server.enqueue(MockResponse().withWebSocketUpgrade(p))

    @Test fun entregaConfirmaEMandaOSegredo() {
        val p = Papel(); ligar(p)
        val l = Escuta(); cliente(l).start()
        val req = server.takeRequest(3, TimeUnit.SECONDS)
        assertEquals("Bearer dpd_segredo", req!!.getHeader("Authorization"))
        assertEquals("/v1/connect", req.path)
        while (p.ws == null) Thread.sleep(10)
        p.empurra("m1")
        val m = assertNotNull(l.mensagens.poll(3, TimeUnit.SECONDS))
        assertEquals("m1", m.id); assertTrue(m.highPriority)
        assertTrue(p.ack()!!.contains("\"id\":\"m1\""))
    }

    @Test fun reenvioNaoRepeteOEfeitoMasConfirma() {
        val p = Papel(); ligar(p)
        val l = Escuta(); cliente(l).start()
        while (p.ws == null) Thread.sleep(10)
        p.empurra("dup"); p.empurra("dup")
        assertNotNull(l.mensagens.poll(3, TimeUnit.SECONDS))
        assertTrue(p.ack() != null && p.ack() != null, "as duas vezes foram confirmadas")
        assertNull(l.mensagens.poll(300, TimeUnit.MILLISECONDS), "o efeito só uma vez")
    }

    @Test fun seOTratamentoFalhaNaoConfirmaEOReenvioConfirma() {
        val p = Papel(); ligar(p)
        val l = Escuta(falhaNa1 = true); cliente(l).start()
        while (p.ws == null) Thread.sleep(10)
        p.empurra("x")
        assertNull(p.ack(), "falhou: sem ack, o servidor reenvia")
        p.empurra("x")
        assertNotNull(l.mensagens.poll(3, TimeUnit.SECONDS))
        assertTrue(p.ack()!!.contains("\"x\""))
    }

    @Test fun voltaALigarDepoisDeCair() {
        val p1 = Papel(); val p2 = Papel(); ligar(p1); ligar(p2)
        val l = Escuta(); cliente(l).start()
        while (p1.ws == null) Thread.sleep(10)
        p1.ws!!.close(1001, "reinicio")
        val t0 = System.currentTimeMillis()
        while (p2.ws == null && System.currentTimeMillis() - t0 < 5000) Thread.sleep(10)
        assertNotNull(p2.ws, "ligou outra vez")
        p2.empurra("depois")
        assertEquals("depois", l.mensagens.poll(3, TimeUnit.SECONDS)?.id)
        assertTrue(l.estados.contains(PushClient.State.Backoff))
    }

    @Test fun segredoRecusadoParaDeTentar() {
        server.enqueue(MockResponse().setResponseCode(401))
        val l = Escuta(); val c = cliente(l); c.start()
        val t0 = System.currentTimeMillis()
        while (!l.auth && System.currentTimeMillis() - t0 < 3000) Thread.sleep(10)
        assertTrue(l.auth); assertEquals(PushClient.State.AuthFailed, c.state)
        Thread.sleep(400)
        assertEquals(1, server.requestCount, "não insiste com um segredo revogado")
    }

    @Test fun pararFechaEEsquece() {
        val p = Papel(); ligar(p)
        val l = Escuta(); val c = cliente(l); c.start()
        while (p.ws == null) Thread.sleep(10)
        c.stop()
        val t0 = System.currentTimeMillis()
        while (c.state != PushClient.State.Stopped && System.currentTimeMillis() - t0 < 2000) Thread.sleep(10)
        assertEquals(PushClient.State.Stopped, c.state)
        Thread.sleep(300)
        assertEquals(1, server.requestCount)
    }
}
