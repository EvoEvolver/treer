package ai.treer.mobile.ui.webview

import android.annotation.SuppressLint
import android.content.Intent
import android.net.Uri
import android.webkit.CookieManager
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import android.webkit.WebSettings
import android.webkit.WebView
import android.webkit.WebViewClient
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.viewinterop.AndroidView
import androidx.webkit.WebViewAssetLoader
import ai.treer.mobile.data.TreerApi
import java.io.ByteArrayInputStream

private const val INDEX = "https://${AgentUiBridge.HOST}${AgentUiBridge.PREFIX}index.html"

@SuppressLint("SetJavaScriptEnabled")
@Composable
fun AgentWebViewScreen(
    api: TreerApi,
    baseUrl: String,
    token: String,
    workspaceId: String,
    agentId: String,
    @Suppress("UNUSED_PARAMETER") uiPath: String?,
) {
    val scope = rememberCoroutineScope()
    var loading by remember { mutableStateOf(true) }
    var hosted by remember { mutableStateOf<WebView?>(null) }
    DisposableEffect(hosted) {
        onDispose { (hosted?.tag as? AgentUiBridge)?.dispose() }
    }
    Column(Modifier.fillMaxSize().testTag("agent-ui-webview")) {
        if (loading) LinearProgressIndicator(modifier = Modifier.fillMaxWidth())
        AndroidView(
            modifier = Modifier.fillMaxSize(),
            factory = { context ->
                val assetLoader = WebViewAssetLoader.Builder()
                    .setDomain(AgentUiBridge.HOST)
                    .addPathHandler("/assets/", WebViewAssetLoader.AssetsPathHandler(context))
                    .build()
                WebView(context).apply {
                    WebView.setWebContentsDebuggingEnabled(true)
                    settings.javaScriptEnabled = true
                    settings.domStorageEnabled = true
                    settings.databaseEnabled = true
                    settings.cacheMode = WebSettings.LOAD_NO_CACHE
                    settings.mixedContentMode = WebSettings.MIXED_CONTENT_ALWAYS_ALLOW
                    settings.allowFileAccess = false
                    CookieManager.getInstance().setAcceptCookie(true)
                    CookieManager.getInstance().setAcceptThirdPartyCookies(this, true)
                    injectSessionCookie(baseUrl, token)
                    val bridge = AgentUiBridge(this, api, baseUrl, token, workspaceId, agentId, scope)
                    tag = bridge
                    addJavascriptInterface(bridge, "TreerNative")
                    webViewClient = object : WebViewClient() {
                        override fun shouldInterceptRequest(view: WebView, request: WebResourceRequest): WebResourceResponse? {
                            val url = request.url ?: return null
                            if (url.host == AgentUiBridge.HOST) {
                                val path = url.encodedPath.orEmpty()
                                val isAsset = path == "/assets/agent-ui/" ||
                                    path == "/assets/agent-ui/index.html" ||
                                    path.startsWith("/assets/agent-ui/assets/")
                                if (isAsset) {
                                    val loaded = assetLoader.shouldInterceptRequest(url)
                                    if (loaded != null && path.endsWith("index.html")) {
                                        return injectBridge(loaded)
                                    }
                                    return loaded
                                }
                                if (request.method.equals("GET", true) || request.method.equals("HEAD", true)) {
                                    return runCatching {
                                        val forwarded = api.forwardUiRequest(
                                            baseUrl = baseUrl,
                                            token = token,
                                            workspaceId = workspaceId,
                                            agentId = agentId,
                                            method = request.method,
                                            pathAndQuery = AgentUiBridge.tunnelPath(url.toString()),
                                            headers = request.requestHeaders,
                                            body = null,
                                            contentType = request.requestHeaders["Content-Type"],
                                        )
                                        WebResourceResponse(
                                            forwarded.mimeType,
                                            forwarded.encoding,
                                            forwarded.status,
                                            if (forwarded.status in 200..399) "OK" else "Error",
                                            forwarded.headers,
                                            ByteArrayInputStream(forwarded.body),
                                        )
                                    }.getOrNull()
                                }
                            }
                            return null
                        }

                        override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest): Boolean {
                            val host = request.url.host
                            if (host == AgentUiBridge.HOST) return false
                            val proxyHost = runCatching { Uri.parse(baseUrl).host }.getOrNull()
                            if (host != null && host == proxyHost) return false
                            return runCatching {
                                view.context.startActivity(Intent(Intent.ACTION_VIEW, request.url))
                                true
                            }.getOrDefault(false)
                        }

                        override fun onPageFinished(view: WebView, url: String) {
                            loading = false
                        }
                    }
                    loadUrl("$INDEX?agent=${Uri.encode(agentId)}")
                    hosted = this
                }
            },
            update = { view ->
                hosted = view
            },
        )
    }
}

private fun injectSessionCookie(baseUrl: String, token: String) {
    val uri = Uri.parse(baseUrl)
    val host = uri.host ?: return
    val secure = if (uri.scheme == "https") "Secure;" else ""
    CookieManager.getInstance().setCookie(baseUrl, "treer_session=$token; Path=/; $secure")
    CookieManager.getInstance().setCookie("https://$host", "treer_session=$token; Path=/; $secure")
    CookieManager.getInstance().flush()
}

private fun injectBridge(original: WebResourceResponse): WebResourceResponse {
    val html = original.data?.bufferedReader(Charsets.UTF_8)?.use { it.readText() } ?: return original
    val injected = html.replaceFirst("<head>", "<head>\n$BRIDGE_SCRIPT")
    return WebResourceResponse(
        "text/html",
        "utf-8",
        original.statusCode.takeIf { it > 0 } ?: 200,
        original.reasonPhrase?.ifBlank { "OK" } ?: "OK",
        original.responseHeaders,
        ByteArrayInputStream(injected.toByteArray(Charsets.UTF_8)),
    )
}

private const val BRIDGE_SCRIPT = """
<script>
(function() {
  if (window.__treerBridgeInstalled) return;
  window.__treerBridgeInstalled = true;
  const pending = {};
  window.__treerNativeHttp = function(result) {
    const waiter = pending[result.id];
    if (!waiter) return;
    delete pending[result.id];
    waiter(result);
  };
  const sockets = {};
  window.__treerNativeWs = function(event) {
    const socket = sockets[event.id];
    if (!socket) return;
    if (event.type === 'open') {
      socket.readyState = 1;
      socket.onopen && socket.onopen({});
    } else if (event.type === 'message') {
      socket.onmessage && socket.onmessage({ data: event.data });
    } else if (event.type === 'error') {
      socket.readyState = 3;
      socket.onerror && socket.onerror({ message: event.data });
    } else if (event.type === 'close') {
      socket.readyState = 3;
      socket.onclose && socket.onclose({ code: 1000, reason: event.data });
    }
  };
  function shouldForward(url) {
    try {
      const parsed = new URL(url, window.location.href);
      return parsed.host === 'appassets.androidplatform.net';
    } catch (e) {
      return true;
    }
  }
  const origFetch = window.fetch.bind(window);
  window.fetch = function(input, init) {
    const url = typeof input === 'string' ? input : (input && input.url) || String(input);
    if (!shouldForward(url) || String(url).includes('/assets/agent-ui/assets/')) {
      return origFetch(input, init);
    }
    const method = (init && init.method) || (input && input.method) || 'GET';
    const headers = {};
    const rawHeaders = (init && init.headers) || (input && input.headers);
    if (rawHeaders) {
      if (typeof rawHeaders.forEach === 'function') {
        rawHeaders.forEach((value, key) => { headers[key] = value; });
      } else {
        Object.assign(headers, rawHeaders);
      }
    }
    const body = (init && init.body) ? String(init.body) : '';
    const id = 'h' + Date.now() + Math.random().toString(16).slice(2);
    return new Promise((resolve, reject) => {
      pending[id] = function(result) {
        if (!result.ok && result.status >= 500 && !result.body) {
          reject(new Error(result.body || 'native forward failed'));
          return;
        }
        resolve(new Response(result.body || '', {
          status: result.status || 200,
          headers: { 'content-type': result.contentType || 'application/json' }
        }));
      };
      TreerNative.http(id, method, url, JSON.stringify(headers), body);
    });
  };
  const NativeWebSocket = window.WebSocket;
  window.WebSocket = function(url, protocols) {
    const href = String(url);
    if (!shouldForward(href)) {
      return new NativeWebSocket(url, protocols);
    }
    const id = 'w' + Date.now() + Math.random().toString(16).slice(2);
    const socket = {
      readyState: 0,
      bufferedAmount: 0,
      url: href,
      onopen: null,
      onmessage: null,
      onerror: null,
      onclose: null,
      send: function(data) { TreerNative.wsSend(id, typeof data === 'string' ? data : String(data)); },
      close: function(code, reason) { TreerNative.wsClose(id, code || 1000, reason || ''); },
      addEventListener: function(type, fn) { this['on' + type] = fn; },
      removeEventListener: function() {}
    };
    sockets[id] = socket;
    TreerNative.wsOpen(id, href);
    return socket;
  };
  window.WebSocket.CONNECTING = 0;
  window.WebSocket.OPEN = 1;
  window.WebSocket.CLOSING = 2;
  window.WebSocket.CLOSED = 3;
})();
</script>
"""
