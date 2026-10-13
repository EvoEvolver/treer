package ai.treer.mobile.ui.webview

import java.net.URI

internal fun sessionCookieHeader(baseUrl: String, token: String): String? {
    val uri = runCatching { URI(baseUrl) }.getOrNull() ?: return null
    if (uri.scheme !in listOf("http", "https") || uri.host == null || uri.userInfo != null ||
        !token.matches(Regex("^[A-Za-z0-9_.~-]+$"))) return null
    return "treer_session=$token; Path=/; HttpOnly; SameSite=Strict" +
        if (uri.scheme == "https") "; Secure" else ""
}
