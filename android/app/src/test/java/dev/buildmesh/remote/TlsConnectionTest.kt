package dev.buildmesh.remote

import java.util.concurrent.TimeUnit
import kotlinx.coroutines.runBlocking
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.tls.HandshakeCertificates
import okhttp3.tls.HeldCertificate
import org.junit.Assert.*
import org.junit.Test

class TlsConnectionTest {
    @Test fun repeatedTrustedRequestsReturnIndependentResponses() = runBlocking {
        val root = HeldCertificate.Builder().certificateAuthority(1).build()
        val leaf = HeldCertificate.Builder().commonName("localhost").addSubjectAlternativeName("localhost").signedBy(root).build()
        val identity = HandshakeCertificates.Builder().heldCertificate(leaf, root.certificate).build()
        MockWebServer().use { server ->
            server.useHttps(identity.sslSocketFactory(), false)
            repeat(5) { server.enqueue(MockResponse().setBody("{\"index\":$it}")) }
            val api = BuildmeshApi("https://localhost:${server.port}", root.certificate.encoded)
            try {
                repeat(5) {
                    assertEquals(it.toLong(), api.get("/status").obj().number("index"))
                    val request = server.takeRequest(5, TimeUnit.SECONDS)!!
                    assertEquals("/status", request.path)
                    assertNotNull(request.handshake)
                }
                assertEquals(5, server.requestCount)
            } finally { api.close() }
        }
    }
}
