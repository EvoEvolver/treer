package ai.treer.mobile

import ai.treer.mobile.data.TreerApi
import com.sun.net.httpserver.HttpServer
import java.net.InetSocketAddress
import java.util.Collections
import org.junit.Assert.*
import org.junit.Test

class NativeAuthTest {
    @Test fun passwordResetRequestsUseNativePreAuthWithoutBearerOrCookies() {
        val received = Collections.synchronizedList(mutableListOf<Map<String, String?>>())
        val server = HttpServer.create(InetSocketAddress("127.0.0.1", 0), 0)
        server.createContext("/api/auth/") { exchange ->
            received.add(mapOf(
                "path" to exchange.requestURI.path,
                "method" to exchange.requestMethod,
                "client" to exchange.requestHeaders.getFirst("X-Treer-Client"),
                "authorization" to exchange.requestHeaders.getFirst("Authorization"),
                "cookie" to exchange.requestHeaders.getFirst("Cookie"),
                "origin" to exchange.requestHeaders.getFirst("Origin"),
            ))
            exchange.requestBody.use { it.readBytes() }
            val response = "{\"ok\":true}".toByteArray()
            exchange.sendResponseHeaders(200, response.size.toLong())
            exchange.responseBody.use { it.write(response) }
        }
        server.start()
        try {
            val api = TreerApi()
            val base = "http://127.0.0.1:${server.address.port}"
            api.requestPasswordReset(base, "synthetic@example.test")
            api.resetPassword(base, "synthetic-reset", "synthetic-password")
            assertEquals(listOf("/api/auth/request-password-reset", "/api/auth/reset-password"), received.map { it["path"] })
            received.forEach { request ->
                assertEquals("POST", request["method"])
                assertEquals("mobile_android", request["client"])
                assertNull(request["authorization"])
                assertNull(request["cookie"])
                assertNull(request["origin"])
            }
        } finally { server.stop(0) }
    }
}
