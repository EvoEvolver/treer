package ai.treer.mobile.ui.screens

import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import ai.treer.mobile.data.TreerApi
import ai.treer.mobile.domain.TerminalBinaryFrame
import ai.treer.mobile.domain.TerminalBinaryKind
import ai.treer.mobile.domain.stripAnsi
import android.os.Handler
import android.os.Looper
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import okio.ByteString
import org.json.JSONObject

@Composable
fun AgentTerminalScreen(
    api: TreerApi,
    baseUrl: String,
    token: String,
    workspaceId: String,
    agentId: String,
) {
    var output by remember { mutableStateOf("") }
    var status by remember { mutableStateOf("connecting") }
    var socket by remember { mutableStateOf<WebSocket?>(null) }
    val buffer = remember { StringBuilder() }
    val scroll = rememberScrollState()

    DisposableEffect(agentId, workspaceId, token, baseUrl) {
        val main = Handler(Looper.getMainLooper())
        fun append(extra: String) {
            main.post {
                synchronized(buffer) {
                    buffer.append(extra)
                    if (buffer.length > 32_000) {
                        buffer.delete(0, buffer.length - 32_000)
                    }
                    output = buffer.toString()
                }
            }
        }
        val listener = object : WebSocketListener() {
            override fun onOpen(webSocket: WebSocket, response: Response) {
                main.post { status = "live" }
            }

            override fun onMessage(webSocket: WebSocket, textMessage: String) {
                val type = runCatching { JSONObject(textMessage).optString("type") }.getOrDefault("")
                when (type) {
                    "ready" -> main.post { status = "live" }
                    "closed" -> main.post { status = "closed" }
                    "error" -> {
                        main.post { status = "error" }
                        val message = runCatching {
                            JSONObject(textMessage).optJSONObject("error")?.optString("message")
                        }.getOrNull()
                        if (!message.isNullOrBlank()) append("\n[treer] $message")
                    }
                    else -> append(stripAnsi(textMessage))
                }
            }

            override fun onMessage(webSocket: WebSocket, bytes: ByteString) {
                val payload = bytes.toByteArray()
                val frame = TerminalBinaryFrame.tryDecode(payload)
                if (frame != null) {
                    when (frame.kind) {
                        TerminalBinaryKind.Ready, TerminalBinaryKind.Output -> {
                            main.post { status = "live" }
                            append(stripAnsi(String(frame.payload, Charsets.UTF_8)))
                        }
                        TerminalBinaryKind.Input -> Unit
                    }
                } else {
                    append(stripAnsi(String(payload, Charsets.UTF_8)))
                }
            }

            override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
                main.post { status = "error" }
                append("\n[treer] ${t.message ?: "terminal error"}")
            }

            override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
                main.post { status = "closed" }
            }
        }
        val created = api.terminalSocket(baseUrl, token, workspaceId, agentId, 80, 24, listener)
        socket = created
        onDispose {
            created.close(1000, "leave")
            socket = null
        }
    }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .background(Color(0xFF0F1215))
            .testTag("agent-terminal"),
    ) {
        Text(
            "Emergency TUI · $status",
            color = Color(0xFFD8DCDF),
            modifier = Modifier.padding(12.dp),
            style = MaterialTheme.typography.labelMedium,
        )
        Text(
            output.ifBlank { "Waiting for output…" },
            color = Color(0xFFD8DCDF),
            fontFamily = FontFamily.Monospace,
            fontSize = 12.sp,
            modifier = Modifier
                .weight(1f)
                .fillMaxWidth()
                .verticalScroll(scroll)
                .horizontalScroll(rememberScrollState())
                .padding(12.dp),
        )
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(8.dp),
            horizontalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            KeyButton("Esc") { sendBytes(socket, "\u001B") }
            KeyButton("Tab") { sendBytes(socket, "\t") }
            KeyButton("^C") { sendBytes(socket, "\u0003") }
            KeyButton("↑") { sendBytes(socket, "\u001B[A") }
            KeyButton("↓") { sendBytes(socket, "\u001B[B") }
            KeyButton("←") { sendBytes(socket, "\u001B[D") }
            KeyButton("→") { sendBytes(socket, "\u001B[C") }
            KeyButton("Enter") { sendBytes(socket, "\r") }
        }
    }
}

@Composable
private fun KeyButton(label: String, onClick: () -> Unit) {
    OutlinedButton(onClick = onClick) { Text(label) }
}

private fun sendBytes(socket: WebSocket?, value: String) {
    socket?.send(ByteString.of(*value.toByteArray(Charsets.UTF_8)))
}
