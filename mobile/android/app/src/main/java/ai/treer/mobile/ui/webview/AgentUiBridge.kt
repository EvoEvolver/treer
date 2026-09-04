package ai.treer.mobile.ui.webview

import android.webkit.JavascriptInterface
import android.webkit.WebView
import ai.treer.mobile.data.TreerApi
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import okio.ByteString
import org.json.JSONObject
import java.util.concurrent.ConcurrentHashMap

class AgentUiBridge(
    private val webView: WebView,
    private val api: TreerApi,
    private val baseUrl: String,
    private val token: String,
    private val workspaceId: String,
    private val agentId: String,
    private val scope: CoroutineScope,
) {
    private val sockets = ConcurrentHashMap<String, WebSocket>()

    @JavascriptInterface
    fun http(requestId: String, method: String, url: String, headersJson: String, body: String) {
        scope.launch {
            val result = withContext(Dispatchers.IO) {
                runCatching {
                    val headers = parseHeaders(headersJson)
                    val path = tunnelPath(url)
                    val forwarded = api.forwardUiRequest(
                        baseUrl = baseUrl,
                        token = token,
                        workspaceId = workspaceId,
                        agentId = agentId,
                        method = method,
                        pathAndQuery = path,
                        headers = headers,
                        body = body.toByteArray(Charsets.UTF_8),
                        contentType = headers.entries.firstOrNull { it.key.equals("content-type", true) }?.value,
                    )
                    JSONObject()
                        .put("id", requestId)
                        .put("ok", true)
                        .put("status", forwarded.status)
                        .put("body", String(forwarded.body, Charsets.UTF_8))
                        .put("contentType", forwarded.mimeType)
                }.getOrElse { error ->
                    JSONObject()
                        .put("id", requestId)
                        .put("ok", false)
                        .put("status", 502)
                        .put("body", error.message ?: "forward failed")
                        .put("contentType", "text/plain")
                }
            }
            withContext(Dispatchers.Main) {
                webView.evaluateJavascript("window.__treerNativeHttp && window.__treerNativeHttp(${result.toString()})", null)
            }
        }
    }

    @JavascriptInterface
    fun wsOpen(socketId: String, url: String) {
        val path = tunnelPath(url)
        val socket = api.uiTunnelSocket(
            baseUrl = baseUrl,
            token = token,
            workspaceId = workspaceId,
            agentId = agentId,
            pathAndQuery = path,
            listener = object : WebSocketListener() {
                override fun onOpen(webSocket: WebSocket, response: Response) {
                    postEvent(socketId, "open", "")
                }

                override fun onMessage(webSocket: WebSocket, text: String) {
                    postEvent(socketId, "message", text)
                }

                override fun onMessage(webSocket: WebSocket, bytes: ByteString) {
                    postEvent(socketId, "message", bytes.utf8())
                }

                override fun onClosing(webSocket: WebSocket, code: Int, reason: String) {
                    webSocket.close(code, reason)
                }

                override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
                    sockets.remove(socketId)
                    postEvent(socketId, "close", reason)
                }

                override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
                    sockets.remove(socketId)
                    postEvent(socketId, "error", t.message ?: "socket failed")
                }
            },
        )
        sockets[socketId] = socket
    }

    @JavascriptInterface
    fun wsSend(socketId: String, data: String) {
        sockets[socketId]?.send(data)
    }

    @JavascriptInterface
    fun wsClose(socketId: String, code: Int, reason: String) {
        sockets.remove(socketId)?.close(code.takeIf { it in 1000..4999 } ?: 1000, reason)
    }

    fun dispose() {
        sockets.values.forEach { it.close(1000, "leave") }
        sockets.clear()
    }

    private fun postEvent(socketId: String, type: String, data: String) {
        val payload = JSONObject()
            .put("id", socketId)
            .put("type", type)
            .put("data", data)
            .toString()
        webView.post {
            webView.evaluateJavascript("window.__treerNativeWs && window.__treerNativeWs($payload)", null)
        }
    }

    companion object {
        const val HOST = "appassets.androidplatform.net"
        const val PREFIX = "/assets/agent-ui/"

        fun tunnelPath(url: String): String {
            val parsed = runCatching { java.net.URI(url) }.getOrNull()
            val path = parsed?.path.orEmpty()
            val query = parsed?.rawQuery
            val relative = when {
                path.startsWith(PREFIX) -> path.removePrefix(PREFIX)
                path.startsWith("/assets/agent-ui") -> path.removePrefix("/assets/agent-ui").trimStart('/')
                else -> path.trimStart('/')
            }.ifBlank { "" }
            return if (query.isNullOrBlank()) relative else "$relative?$query"
        }
    }
}

private fun parseHeaders(raw: String): Map<String, String> {
    if (raw.isBlank()) return emptyMap()
    return runCatching {
        val obj = JSONObject(raw)
        obj.keys().asSequence().associateWith { obj.getString(it) }
    }.getOrDefault(emptyMap())
}
