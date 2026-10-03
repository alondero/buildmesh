package dev.buildmesh.remote

import java.net.URI
import java.net.URLDecoder
import java.security.KeyStore
import java.security.MessageDigest
import java.security.cert.CertificateFactory
import java.security.cert.X509Certificate
import java.util.concurrent.TimeUnit
import javax.net.ssl.SSLContext
import javax.net.ssl.TrustManagerFactory
import javax.net.ssl.X509TrustManager
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.OkHttpClient
import okhttp3.Request

data class Invitation(val origin: String, val ticket: String, val fingerprint: String?) {
    companion object {
        fun parse(value: String, manualFingerprint: String = ""): Invitation {
            val uri = URI(value.trim())
            require(uri.userInfo == null && uri.host != null) { "Paste the full pairing URL from Buildmesh." }
            require(uri.scheme == "https" || (uri.scheme == "http" && uri.host in setOf("127.0.0.1", "localhost", "10.0.2.2"))) {
                "Remote connections require an HTTPS pairing URL."
            }
            require(uri.rawQuery == null && (uri.path.isNullOrEmpty() || uri.path in setOf("/", "/v2", "/v2/"))) {
                "Use the pairing URL shown in Remote Access."
            }
            val parts = (uri.rawFragment ?: "").split('&').associate {
                val pair = it.split('=', limit = 2)
                decode(pair[0]) to decode(pair.getOrElse(1) { "" })
            }
            val ticket = parts["pair"].orEmpty()
            require(ticket.matches(Regex("[A-Za-z0-9_-]{16,256}"))) { "The URL needs a fresh #pair invitation from the desktop." }
            val fingerprint = (parts["ca"] ?: manualFingerprint).takeIf { it.isNotBlank() }?.let(::normalizeFingerprint)
            require(uri.scheme != "https" || fingerprint != null) {
                "Paste the Root CA SHA-256 fingerprint from the desktop's Certificate section."
            }
            val origin = uri.toString().substringBefore('#').trimEnd('/').removeSuffix("/v2")
            return Invitation(origin, ticket, fingerprint)
        }

        private fun decode(value: String) = URLDecoder.decode(value, "UTF-8")
    }
}

fun normalizeFingerprint(value: String): String {
    val normalized = value.replace(":", "").replace(" ", "").lowercase()
    require(normalized.matches(Regex("[a-f0-9]{64}"))) { "The Root CA fingerprint must contain 64 hexadecimal digits." }
    return normalized
}

fun verifiedRoot(bytes: ByteArray, fingerprint: String): X509Certificate {
    val actual = MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }
    require(MessageDigest.isEqual(actual.toByteArray(), normalizeFingerprint(fingerprint).toByteArray())) {
        "Certificate fingerprint does not match the desktop. Scan a fresh QR code."
    }
    val certificate = CertificateFactory.getInstance("X.509").generateCertificate(bytes.inputStream()) as X509Certificate
    certificate.checkValidity()
    require(certificate.basicConstraints >= 0) { "Buildmesh must provide a root CA certificate." }
    certificate.verify(certificate.publicKey)
    return certificate
}

fun httpClient(root: X509Certificate? = null): OkHttpClient {
    val builder = OkHttpClient.Builder()
        .connectTimeout(10, TimeUnit.SECONDS).readTimeout(30, TimeUnit.SECONDS)
        .callTimeout(45, TimeUnit.SECONDS).pingInterval(20, TimeUnit.SECONDS)
        .followRedirects(false).followSslRedirects(false).retryOnConnectionFailure(false)
    if (root != null) {
        val keys = KeyStore.getInstance(KeyStore.getDefaultType()).apply { load(null); setCertificateEntry("desktop", root) }
        val manager = TrustManagerFactory.getInstance(TrustManagerFactory.getDefaultAlgorithm()).apply { init(keys) }
            .trustManagers.filterIsInstance<X509TrustManager>().single()
        val context = SSLContext.getInstance("TLS").apply { init(null, arrayOf(manager), null) }
        builder.sslSocketFactory(context.socketFactory, manager)
    }
    // OkHttp's default hostname verifier remains active for all authenticated requests.
    return builder.build()
}

/** Fetch only public certificate bytes; their QR fingerprint authenticates this bootstrap. */
@android.annotation.SuppressLint("CustomX509TrustManager") // Certificate-only bootstrap is authenticated by verifiedRoot before any session request.
fun downloadRoot(invitation: Invitation): ByteArray {
    require(invitation.origin.startsWith("https://") && invitation.fingerprint != null)
    val bootstrap = object : X509TrustManager {
        override fun getAcceptedIssuers(): Array<X509Certificate> = emptyArray()
        override fun checkClientTrusted(chain: Array<X509Certificate>, authType: String) = error("Client certificates are unsupported")
        override fun checkServerTrusted(chain: Array<X509Certificate>, authType: String) {
            require(chain.isNotEmpty())
            chain.forEach { it.checkValidity() }
        }
    }
    // This isolated client has no cookie jar, credentials, redirects or other API paths.
    // Hostname verification still applies; the returned bytes cannot become trust without the QR hash.
    val context = SSLContext.getInstance("TLS").apply { init(null, arrayOf(bootstrap), null) }
    val client = httpClient().newBuilder().sslSocketFactory(context.socketFactory, bootstrap).build()
    try {
        val request = Request.Builder().url(invitation.origin.toHttpUrl().resolve("/install-cert.der")!!).build()
        return client.newCall(request).execute().use { response ->
            check(response.code == 200) { "The desktop could not provide its root certificate (${response.code})." }
            val bytes = response.body!!.byteStream().use { stream ->
                val output = java.io.ByteArrayOutputStream()
                val buffer = ByteArray(1024)
                while (output.size() <= 16_384) {
                    val count = stream.read(buffer)
                    if (count < 0) break
                    output.write(buffer, 0, count)
                }
                output.toByteArray()
            }
            require(bytes.size <= 16_384) { "The certificate response is too large." }
            verifiedRoot(bytes, invitation.fingerprint)
            bytes
        }
    } finally {
        client.connectionPool.evictAll()
        client.dispatcher.executorService.shutdown()
    }
}
