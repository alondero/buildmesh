package dev.buildmesh.remote

import java.io.IOException
import java.security.cert.CertificateFactory
import java.security.cert.X509Certificate
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import kotlinx.coroutines.delay
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.*
import okhttp3.*
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.RequestBody.Companion.toRequestBody

fun JsonElement.obj() = this as JsonObject
fun JsonObject.text(key: String) = (this[key] as? JsonPrimitive)?.contentOrNull.orEmpty()
fun JsonObject.number(key: String) = (this[key] as? JsonPrimitive)?.longOrNull ?: 0L
fun JsonObject.flag(key: String) = (this[key] as? JsonPrimitive)?.booleanOrNull ?: false
fun JsonObject.objectAt(key: String) = this[key] as? JsonObject
fun JsonObject.arrayAt(key: String) = (this[key] as? JsonArray)?.map { it.obj() }.orEmpty()
fun JsonElement.objects() = (this as JsonArray).map { it.obj() }

class ApiException(val status: Int, message: String) : IOException(message) {
    val unauthorized get() = status == 401 || status == 403
}

class BuildmeshApi(
    origin: String,
    private val root: ByteArray? = null,
    initialCookie: String = "",
    private val persist: (DeviceSession) -> Unit = {},
    client: OkHttpClient? = null,
) {
    val origin = origin.trimEnd('/')
    private val base = this.origin.toHttpUrl()
    @Volatile private var credential = initialCookie
    private val cookieJar = object : CookieJar {
        override fun loadForRequest(url: HttpUrl): List<Cookie> =
            if (sameOrigin(url) && credential.isNotEmpty()) listOfNotNull(Cookie.parse(base, credential)) else emptyList()
        override fun saveFromResponse(url: HttpUrl, cookies: List<Cookie>) {
            if (!sameOrigin(url)) return
            cookies.firstOrNull { it.name == "bm_session" && it.hostOnly && it.domain == base.host && it.httpOnly && (base.scheme != "https" || it.secure) }?.let {
                credential = it.toString()
                persist(session())
            }
        }
    }
    val http = (client ?: httpClient(root?.let {
        CertificateFactory.getInstance("X.509").generateCertificate(it.inputStream()) as X509Certificate
    })).newBuilder().cookieJar(cookieJar).followRedirects(false).followSslRedirects(false).retryOnConnectionFailure(false).build()

    private fun sameOrigin(url: HttpUrl) = url.host == base.host && url.port == base.port && url.scheme == base.scheme
    fun session(): DeviceSession {
        check(credential.isNotEmpty()) { "The desktop did not return a device session." }
        return DeviceSession(origin, root, credential)
    }

    suspend fun pair(ticket: String) {
        exchange("/api/pair", ticket)
        session()
    }
    suspend fun restore() { exchange("/api/session", null) }

    private suspend fun exchange(path: String, ticket: String?) = withContext(Dispatchers.IO) {
        val request = request(path).post(ByteArray(0).toRequestBody()).apply {
            ticket?.let { header("Authorization", "Bearer $it") }
        }.build()
        execute(request).use { response ->
            ensureSuccess(response)
            check(response.code == 204) { "Unexpected session response (${response.code})." }
        }
    }

    // The embedded server serves one HTTP request per connection.
    private fun request(path: String) = Request.Builder().url(base.resolve(path)!!)
        .header("User-Agent", "Buildmesh Android/1.0").header("Connection", "close")

    suspend fun get(path: String): JsonElement = json(request(path).build())
    suspend fun post(path: String, body: JsonObject): JsonElement = json(request(path).post(body.toString().toRequestBody("application/json".toMediaType())).build())

    private suspend fun json(request: Request): JsonElement = withContext(Dispatchers.IO) { execute(request).use { response ->
        ensureSuccess(response)
        val body = response.body?.string().orEmpty()
        if (body.isBlank()) JsonNull else Json.parseToJsonElement(body)
    } }

    private fun ensureSuccess(response: Response) {
        if (response.code == 207) {
            val partial = Json.parseToJsonElement(response.body!!.string()).obj()
            throw ApiException(207, "Node ${partial.objectAt("node")?.number("id")} was imported, but could not start: ${partial.text("spawn_error")}. Check Work before retrying.")
        }
        if (!response.isSuccessful) {
            val detail = runCatching { Json.parseToJsonElement(response.body!!.string()).obj().text("error") }.getOrDefault("")
            throw ApiException(response.code, detail.ifEmpty {
                if (response.code in setOf(401, 403)) "This device is no longer authorized. Pair again from the desktop."
                else "Desktop request failed (${response.code})."
            })
        }
    }

    private suspend fun execute(request: Request): Response = suspendCancellableCoroutine { continuation ->
        val call = http.newCall(request)
        continuation.invokeOnCancellation { call.cancel() }
        call.enqueue(object : Callback {
            override fun onFailure(call: Call, e: IOException) {
                if (!continuation.isCancelled) continuation.resumeWithException(e)
            }
            override fun onResponse(call: Call, response: Response) {
                continuation.resume(response) { _, value, _ -> value.close() }
            }
        })
    }

    suspend fun ticket(surface: String, nodeId: Long? = null): String {
        val body = buildJsonObject { put("surface", surface); put("node_id", nodeId?.let(::JsonPrimitive) ?: JsonNull) }
        for (attempt in 0..1) {
            val result = withContext(Dispatchers.IO) { execute(request("/api/ws-ticket").post(body.toString().toRequestBody("application/json".toMediaType())).build()).use {
                if (it.code != 429) {
                    ensureSuccess(it)
                    val ticket = Json.parseToJsonElement(it.body!!.string()).obj().text("ticket").also { value -> check(value.isNotEmpty()) }
                    return@withContext TicketAttempt(ticket, 0)
                }
                if (attempt == 1) throw ApiException(429, "The desktop is busy. Reconnecting shortly.")
                TicketAttempt(null, (it.header("Retry-After")?.toLongOrNull()?.coerceIn(0, 2) ?: 1) * 1000)
            } }
            result.ticket?.let { return it }
            delay(result.retryMillis)
        }
        error("Unreachable")
    }

    private data class TicketAttempt(val ticket: String?, val retryMillis: Long)

    fun socketRequest(surface: String, ticket: String, nodeId: Long? = null): Request {
        val path = if (surface == "events") "/ws/events" else "/ws/terminal/$nodeId"
        val url = base.resolve(path)!!.newBuilder().addQueryParameter("ticket", ticket).build()
        return Request.Builder().url(url).header("User-Agent", "Buildmesh Android/1.0").build()
    }

    suspend fun sendKeys(id: Long, sequence: String) {
        val body = inputBody(sequence)
        post("/api/nodes/$id/input", body)
    }

    fun close() {
        http.dispatcher.cancelAll()
        // Conscrypt can write TLS close_notify while evicting pooled sockets.
        http.dispatcher.executorService.execute { http.connectionPool.evictAll() }
    }
}

fun inputBody(sequence: String): JsonObject {
    require(sequence.isNotEmpty()) { "Enter a reply first." }
    return buildJsonObject { put("seq", sequence) }.also {
        require(it.toString().toByteArray(Charsets.UTF_8).size <= 1024) { "Reply is too long. Use the terminal for longer text." }
    }
}

fun nodeLabel(node: JsonObject): String {
    val status = node.text("status")
    val lifecycle = node.objectAt("lifecycle")?.takeIf { it.text("status") == status }
    return when (lifecycle?.text("kind")) {
        "background_running" -> "Waiting for background work"
        "question_requested" -> "Needs an answer"
        "permission_requested" -> "Needs permission"
        else -> when (status) {
            "pending", "spawning" -> "Starting…"
            "awaiting_input" -> "Needs attention"
            "ready" -> "Ready"
            "completed" -> "PR opened"
            else -> status.replaceFirstChar { it.uppercase() }
        }
    }
}
