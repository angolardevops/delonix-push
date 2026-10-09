package ao.ngolacloud.push

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import java.util.concurrent.Executors
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.TimeUnit
import kotlin.random.Random

/** Uma mensagem entregue pelo delonix-push. O [id] é estável entre reenvios: deduplica por ele. */
data class PushMessage(
    val id: String,
    val payload: JsonElement,
    val priority: String,
    val collapseKey: String?,
    val expiresAt: String,
) {
    val highPriority: Boolean get() = priority == "high"
}

/**
 * Recebe as mensagens. Se [onMessage] lançar uma excepção a mensagem NÃO é confirmada e o servidor reenvia
 * (pelo menos uma vez): por isso o tratamento tem de ser idempotente ou apoiar-se no [SeenStore].
 */
interface PushListener {
    fun onMessage(message: PushMessage)
    fun onStateChanged(state: PushClient.State) {}
    /** O servidor recusou o segredo (aparelho revogado): o cliente pára e não volta a tentar. */
    fun onAuthFailed() {}
}

/** Memória dos ids já tratados, para um reenvio não repetir o efeito (a app pode persisti-la). */
interface SeenStore {
    fun seen(id: String): Boolean
    fun remember(id: String)
}

class MemorySeenStore(private val capacity: Int = 1000) : SeenStore {
    private val ids = object : LinkedHashMap<String, Unit>(16, 0.75f, false) {
        override fun removeEldestEntry(eldest: MutableMap.MutableEntry<String, Unit>?) = size > capacity
    }

    @Synchronized override fun seen(id: String) = ids.containsKey(id)
    @Synchronized override fun remember(id: String) { ids[id] = Unit }
}

class PushConfig(
    /** `http(s)://host[:porta]` do delonix-push (o `url` que o Meet devolveu). */
    val baseUrl: String,
    /** O `device_secret` (`dpd_…`). */
    val deviceSecret: String,
    val minBackoffMs: Long = 1_000,
    val maxBackoffMs: Long = 60_000,
    /** Intervalo do `ping` aplicacional (o servidor fecha ligações mudas ao fim de 90 s). */
    val heartbeatMs: Long = 30_000,
    /** Uma ligação que durou pelo menos isto repõe o recuo. */
    val stableAfterMs: Long = 10_000,
)

/**
 * Ligação própria ao delonix-push: WebSocket com *ack*, deduplicação, recuo exponencial com *jitter* e
 * *heartbeat*. Não sabe nada de Android: o `:android` põe-no dentro de um serviço em primeiro plano.
 */
class PushClient(
    private val config: PushConfig,
    private val listener: PushListener,
    private val seen: SeenStore = MemorySeenStore(),
    private val http: OkHttpClient = OkHttpClient.Builder().pingInterval(25, TimeUnit.SECONDS).build(),
    private val random: Random = Random.Default,
) {
    enum class State { Stopped, Connecting, Connected, Backoff, AuthFailed }

    private val json = Json { ignoreUnknownKeys = true }
    private val exec = Executors.newSingleThreadScheduledExecutor { r -> Thread(r, "delonix-push").apply { isDaemon = true } }
    private var socket: WebSocket? = null
    private var retry: ScheduledFuture<*>? = null
    private var beat: ScheduledFuture<*>? = null
    private var attempt = 0
    private var openedAt = 0L

    @Volatile var state: State = State.Stopped
        private set

    fun start() = exec.execute {
        if (state != State.Stopped && state != State.AuthFailed) return@execute
        attempt = 0
        connect()
    }

    fun stop() = exec.execute {
        retry?.cancel(false); beat?.cancel(false)
        socket?.close(1000, "stop"); socket = null
        set(State.Stopped)
    }

    /** Liberta a thread. Depois disto o cliente não se reutiliza. */
    fun shutdown() { stop(); exec.shutdown() }

    private fun set(s: State) {
        if (state == s) return
        state = s
        runCatching { listener.onStateChanged(s) }
    }

    private fun connect() {
        set(State.Connecting)
        val wsUrl = config.baseUrl.trimEnd('/').replaceFirst("http", "ws") + "/v1/connect"
        val req = Request.Builder().url(wsUrl).header("Authorization", "Bearer ${config.deviceSecret}").build()
        socket = http.newWebSocket(req, Handler())
    }

    private fun scheduleReconnect() {
        if (state == State.Stopped || state == State.AuthFailed) return
        val stable = openedAt != 0L && System.currentTimeMillis() - openedAt >= config.stableAfterMs
        if (stable) attempt = 0
        openedAt = 0L
        val ceiling = minOf(config.maxBackoffMs, config.minBackoffMs shl minOf(attempt, 16))
        val delay = (ceiling / 2) + random.nextLong(ceiling / 2 + 1) // 50–100 %: evita rebanhos
        attempt++
        set(State.Backoff)
        retry = exec.schedule({ if (state == State.Backoff) connect() }, delay, TimeUnit.MILLISECONDS)
    }

    private inner class Handler : WebSocketListener() {
        override fun onOpen(webSocket: WebSocket, response: Response) = exec.execute {
            if (webSocket !== socket) return@execute
            openedAt = System.currentTimeMillis()
            set(State.Connected)
            beat?.cancel(false)
            beat = exec.scheduleAtFixedRate(
                { socket?.send("""{"type":"ping"}""") }, config.heartbeatMs, config.heartbeatMs, TimeUnit.MILLISECONDS,
            )
        }

        override fun onMessage(webSocket: WebSocket, text: String) = exec.execute {
            if (webSocket !== socket) return@execute
            val obj = runCatching { json.parseToJsonElement(text) as? JsonObject }.getOrNull() ?: return@execute
            if (obj["type"]?.jsonPrimitive?.contentOrNull != "message") return@execute
            val id = obj["id"]?.jsonPrimitive?.contentOrNull ?: return@execute
            if (!seen.seen(id)) {
                val m = PushMessage(
                    id = id,
                    payload = obj["payload"] ?: kotlinx.serialization.json.JsonNull,
                    priority = obj["priority"]?.jsonPrimitive?.contentOrNull ?: "normal",
                    collapseKey = obj["collapse_key"]?.jsonPrimitive?.contentOrNull,
                    expiresAt = obj["expires_at"]?.jsonPrimitive?.contentOrNull ?: "",
                )
                // Sem ack se o tratamento falhar: o servidor reenvia.
                if (runCatching { listener.onMessage(m) }.isFailure) return@execute
                seen.remember(id)
            }
            webSocket.send(buildJsonObject { put("type", "ack"); put("id", id) }.toString())
        }

        // O servidor quer fechar: tem de se responder, senão o OkHttp nunca chega a `onClosed` e não se volta a ligar.
        override fun onClosing(webSocket: WebSocket, code: Int, reason: String) {
            webSocket.close(code, reason)
        }

        override fun onClosed(webSocket: WebSocket, code: Int, reason: String) = exec.execute {
            if (webSocket === socket) { beat?.cancel(false); scheduleReconnect() }
        }

        override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) = exec.execute {
            if (webSocket !== socket) return@execute
            beat?.cancel(false)
            if (response?.code == 401) {
                set(State.AuthFailed)
                runCatching { listener.onAuthFailed() }
                return@execute
            }
            scheduleReconnect()
        }
    }
}
