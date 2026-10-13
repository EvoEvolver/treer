package ai.treer.mobile.ui.webview

import org.junit.Assert.*
import org.junit.Test

class SessionCookieTest {
    @Test fun httpsCookieProtectsSessionAndHasNoBroadDomain() {
        val header = sessionCookieHeader("https://proxy.example.test/", "synthetic-session")!!
        assertTrue(header.contains("HttpOnly"))
        assertTrue(header.contains("SameSite=Strict"))
        assertTrue(header.endsWith("; Secure"))
        assertFalse(header.contains("Domain="))
    }
    @Test fun localHttpRemainsUsableButDoesNotClaimTransportSecurity() {
        val header = sessionCookieHeader("http://127.0.0.1/", "synthetic-session")!!
        assertTrue(header.contains("HttpOnly"))
        assertFalse(header.contains("Secure"))
    }
    @Test fun rejectsHeaderInjectionAndCredentialBearingUrls() {
        assertNull(sessionCookieHeader("https://proxy.example.test", "fake; Domain=example.test"))
        assertNull(sessionCookieHeader("https://proxy.example.test", "fake\n"))
        assertNull(sessionCookieHeader("https://proxy.example.test", "fake\r\n"))
        assertNull(sessionCookieHeader("https://user:fake@proxy.example.test", "synthetic-session"))
        assertNull(sessionCookieHeader("javascript:alert(1)", "synthetic-session"))
    }
}
