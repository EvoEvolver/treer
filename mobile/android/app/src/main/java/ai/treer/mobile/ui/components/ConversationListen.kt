package ai.treer.mobile.ui.components

import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import android.util.Log
import ai.treer.mobile.data.LocalSpeechGate
import ai.treer.mobile.data.SpeechTurn
import ai.treer.mobile.data.SpeechTurnDetector
import ai.treer.mobile.data.TreerApi
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import okio.ByteString
import org.json.JSONObject
import java.util.ArrayDeque

internal class ConversationListen(
    private val onPartial: (String) -> Unit,
    private val onLiveUser: (String) -> Unit,
    private val onStatus: (String) -> Unit,
    private val onError: (String?) -> Unit,
    private val onLevel: (Float) -> Unit,
    var onUtterance: (String) -> Unit,
) {
    private val detector = SpeechTurnDetector()
    private val gate = LocalSpeechGate()
    private var recorderJob: Job? = null
    private var socket: WebSocket? = null
    private var capturing = false
    private var paused = false
    private var stoppingTurn = false
    private val preroll = ArrayDeque<ByteArray>()
    private val holdLines = mutableListOf<String>()
    private var lastPartial = ""

    fun start(scope: kotlinx.coroutines.CoroutineScope, baseUrl: String, token: String?, workspaceId: String?) {
        if (workspaceId.isNullOrBlank() || token.isNullOrBlank()) {
            onError("先登录并选择 workspace。")
            return
        }
        stop()
        paused = false
        detector.reset()
        onStatus("正在听")
        onError(null)
        recorderJob = scope.launch(Dispatchers.IO) {
            val frameBytes = FRAME_SAMPLES * 2
            val minBuf = AudioRecord.getMinBufferSize(
                16000,
                AudioFormat.CHANNEL_IN_MONO,
                AudioFormat.ENCODING_PCM_16BIT,
            ).coerceAtLeast(frameBytes * 8)
            val record = AudioRecord(
                MediaRecorder.AudioSource.MIC,
                16000,
                AudioFormat.CHANNEL_IN_MONO,
                AudioFormat.ENCODING_PCM_16BIT,
                minBuf,
            )
            if (record.state != AudioRecord.STATE_INITIALIZED) {
                withContext(Dispatchers.Main) { onError("无法打开麦克风") }
                return@launch
            }
            try {
                record.startRecording()
                val buffer = ByteArray(frameBytes)
                var lastLevelAt = 0L
                while (isActive) {
                    val read = record.read(buffer, 0, buffer.size)
                    if (read <= 0) continue
                    val frame = if (read == buffer.size) buffer.copyOf() else buffer.copyOf(read)
                    if (paused) {
                        if (capturing) finishTurn(scope)
                        detector.reset()
                        preroll.clear()
                        continue
                    }
                    rememberPreroll(frame)
                    val inspected = gate.inspect(frame)
                    val now = android.os.SystemClock.elapsedRealtime()
                    if (now - lastLevelAt > 80) {
                        lastLevelAt = now
                        val level = (inspected.rms / 4000.0).toFloat().coerceIn(0f, 1f)
                        withContext(Dispatchers.Main) {
                            onLevel(level)
                            if (!capturing) {
                                onStatus(if (inspected.speech || level > 0.08f) "听到声音" else "正在听")
                            }
                        }
                    }
                    when (detector.onFrame(inspected.speech)) {
                        SpeechTurn.Started -> {
                            withContext(Dispatchers.Main) { onStatus("检测到说话") }
                            openAsr(baseUrl, token, workspaceId)
                            flushPreroll()
                            sendPcm(frame)
                        }
                        SpeechTurn.Ended -> {
                            sendPcm(frame)
                            finishTurn(scope)
                        }
                        null -> if (capturing) sendPcm(frame)
                    }
                }
            } catch (ex: Exception) {
                Log.w(TAG, "conversation recorder failed", ex)
                withContext(Dispatchers.Main) { onError(ex.message) }
            } finally {
                runCatching { record.stop() }
                record.release()
            }
        }
    }

    fun setPaused(value: Boolean) {
        paused = value
        if (value) {
            onStatus("等待回复")
        } else if (recorderJob?.isActive == true) {
            detector.reset()
            onStatus("正在听")
        }
    }

    fun stop() {
        recorderJob?.cancel()
        recorderJob = null
        closeSocket()
        capturing = false
        stoppingTurn = false
        detector.reset()
        preroll.clear()
        holdLines.clear()
        lastPartial = ""
    }

    private fun rememberPreroll(frame: ByteArray) {
        preroll.addLast(frame)
        while (preroll.size > PREROLL_FRAMES) preroll.removeFirst()
    }

    private fun flushPreroll() {
        for (frame in preroll) sendPcm(frame)
        preroll.clear()
    }

    private fun sendPcm(frame: ByteArray) {
        socket?.send(ByteString.of(*frame))
    }

    private fun openAsr(baseUrl: String, token: String, workspaceId: String) {
        if (capturing) return
        capturing = true
        stoppingTurn = false
        holdLines.clear()
        lastPartial = ""
        val listener = object : WebSocketListener() {
            override fun onMessage(webSocket: WebSocket, text: String) {
                val obj = runCatching { JSONObject(text) }.getOrNull() ?: return
                main {
                    when (obj.optString("type")) {
                        "partial" -> {
                            lastPartial = obj.optString("text")
                            onPartial(lastPartial)
                            val live = listOf(holdLines.joinToString(" "), lastPartial)
                                .filter { it.isNotBlank() }
                                .joinToString(" ")
                            onLiveUser(live)
                        }
                        "final" -> {
                            val line = obj.optString("text")
                            if (line.isNotBlank()) holdLines.add(line)
                            lastPartial = ""
                            onPartial("")
                            onLiveUser(holdLines.joinToString(" "))
                        }
                        "error" -> onError(obj.optString("message", "ASR failed"))
                    }
                }
            }

            override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
                if (stoppingTurn) return
                Log.w(TAG, "conversation asr failed ${t.message}", t)
                main { onError(t.message ?: "ASR connection failed") }
            }
        }
        socket = TreerApi().voiceAsrSocket(baseUrl, token, workspaceId, listener)
    }

    private fun finishTurn(scope: kotlinx.coroutines.CoroutineScope) {
        if (!capturing || stoppingTurn) return
        stoppingTurn = true
        capturing = false
        socket?.send("""{"type":"stop"}""")
        scope.launch {
            kotlinx.coroutines.delay(1200)
            val text = consume()
            closeSocket()
            stoppingTurn = false
            withContext(Dispatchers.Main) {
                onPartial("")
                onLiveUser("")
                onStatus("正在听")
                if (text.isNotBlank()) onUtterance(text)
            }
        }
    }

    private fun consume(): String {
        val joined = holdLines.joinToString(" ").trim()
        val extra = lastPartial.trim()
        holdLines.clear()
        lastPartial = ""
        return when {
            joined.isNotBlank() && extra.isNotBlank() && !joined.contains(extra) -> "$joined $extra".trim()
            joined.isNotBlank() -> joined
            else -> extra
        }
    }

    private fun closeSocket() {
        socket?.close(1000, "turn")
        socket = null
    }

    private fun main(block: () -> Unit) {
        android.os.Handler(android.os.Looper.getMainLooper()).post(block)
    }

    companion object {
        private const val TAG = "TreerVoice"
        private const val FRAME_SAMPLES = 512
        private const val PREROLL_FRAMES = 13
    }
}
