package ai.treer.mobile.ui.components

import android.Manifest
import android.content.pm.PackageManager
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import android.util.Log
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.waitForUpOrCancellation
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.SheetValue
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import ai.treer.mobile.data.TreerApi
import ai.treer.mobile.domain.VoiceAsrStatus
import ai.treer.mobile.domain.VoiceInputMode
import ai.treer.mobile.domain.VoiceLine
import ai.treer.mobile.domain.VoiceTtsStatus
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import okio.ByteString
import org.json.JSONObject

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun VoicePreviewSheet(
    workspaceId: String?,
    baseUrl: String,
    token: String?,
    asr: VoiceAsrStatus,
    lines: List<VoiceLine>,
    busy: Boolean,
    speaking: Boolean,
    mode: VoiceInputMode,
    tts: VoiceTtsStatus,
    onRefreshAsr: () -> Unit,
    onUtterance: (String) -> Unit,
    onHoldStart: () -> Unit,
    onMode: (VoiceInputMode) -> Unit,
    onInstallTts: () -> Unit,
    onInstallTtsVoice: () -> Unit,
    onOpenTtsSettings: () -> Unit,
    onDismiss: () -> Unit,
) {
    val context = LocalContext.current
    val height = LocalConfiguration.current.screenHeightDp.dp
    var granted by remember {
        mutableStateOf(
            ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) ==
                PackageManager.PERMISSION_GRANTED,
        )
    }
    val launcher = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { ok ->
        granted = ok
    }
    val scope = rememberCoroutineScope()
    var holding by remember { mutableStateOf(false) }
    var partial by remember { mutableStateOf("") }
    var liveUser by remember { mutableStateOf("") }
    var error by remember { mutableStateOf<String?>(null) }
    var listenStatus by remember { mutableStateOf("正在听") }
    var listenLevel by remember { mutableStateOf(0f) }
    val listState = rememberLazyListState()
    val talk = remember {
        HoldToTalkSession(
            onHolding = { holding = it },
            onPartial = { partial = it },
            onFinal = { line ->
                if (line.isNotBlank()) {
                    liveUser = if (liveUser.isBlank()) line else "$liveUser $line"
                }
            },
            onError = { error = it },
        )
    }
    val conversation = remember {
        ConversationListen(
            onPartial = { partial = it },
            onLiveUser = { liveUser = it },
            onStatus = { listenStatus = it },
            onError = { error = it },
            onLevel = { listenLevel = it },
            onUtterance = onUtterance,
        )
    }
    conversation.onUtterance = onUtterance
    val latestUtterance = rememberUpdatedState(onUtterance)
    val latestHoldStart = rememberUpdatedState(onHoldStart)

    DisposableEffect(Unit) {
        onRefreshAsr()
        onDispose {
            talk.shutdown()
            conversation.stop()
        }
    }

    LaunchedEffect(mode, granted, asr.enabled, workspaceId, token, baseUrl) {
        if (mode == VoiceInputMode.Conversation && granted && asr.enabled) {
            conversation.start(scope, baseUrl, token, workspaceId)
        } else {
            conversation.stop()
        }
    }
    LaunchedEffect(busy, speaking) {
        conversation.setPaused(busy || speaking)
    }

    val liveCount = (if (liveUser.isNotBlank() || partial.isNotBlank()) 1 else 0) +
        (if (busy) 1 else 0)
    LaunchedEffect(lines.size, liveCount, partial, busy) {
        val last = lines.size + liveCount - 1
        if (last >= 0) {
            listState.animateScrollToItem(last)
        }
    }

    val sheetState = rememberModalBottomSheetState(
        skipPartiallyExpanded = true,
        confirmValueChange = { value -> !(holding && value == SheetValue.Hidden) },
    )
    ModalBottomSheet(
        onDismissRequest = {
            talk.shutdown()
            conversation.stop()
            onDismiss()
        },
        sheetState = sheetState,
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .height(height * 0.96f)
                .padding(horizontal = 16.dp)
                .padding(bottom = 16.dp)
                .semantics { contentDescription = "voice-preview" },
        ) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text("Voice", style = MaterialTheme.typography.titleLarge, modifier = Modifier.weight(1f))
                TextButton(onClick = {
                    talk.shutdown()
                    conversation.stop()
                    onDismiss()
                }) { Text("Close") }
            }
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                FilterChip(
                    selected = mode == VoiceInputMode.Hold,
                    onClick = { onMode(VoiceInputMode.Hold) },
                    label = { Text("按住说话") },
                )
                FilterChip(
                    selected = mode == VoiceInputMode.Conversation,
                    onClick = { onMode(VoiceInputMode.Conversation) },
                    label = { Text("对话") },
                )
            }
            if (tts.message != null) {
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(vertical = 8.dp)
                        .clip(RoundedCornerShape(10.dp))
                        .background(MaterialTheme.colorScheme.errorContainer)
                        .padding(12.dp),
                ) {
                    Text(tts.message.orEmpty(), color = MaterialTheme.colorScheme.onErrorContainer)
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        if (tts.canInstallEngine) {
                            TextButton(onClick = onInstallTts) { Text("安装朗读引擎") }
                        }
                        if (tts.canInstallVoiceData) {
                            TextButton(onClick = onInstallTtsVoice) { Text("安装语音包") }
                        }
                        if (tts.canOpenSettings) {
                            TextButton(onClick = onOpenTtsSettings) { Text("打开系统设置") }
                        }
                    }
                }
            }
            LazyColumn(
                state = listState,
                modifier = Modifier
                    .weight(1f)
                    .fillMaxWidth()
                    .clip(RoundedCornerShape(10.dp))
                    .background(MaterialTheme.colorScheme.surfaceVariant)
                    .padding(12.dp),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                if (lines.isEmpty() && liveUser.isBlank() && partial.isBlank() && !busy) {
                    item {
                        Text(
                            if (!granted) {
                                "需要麦克风权限。"
                            } else if (!asr.enabled) {
                                "ASR 未配置。"
                            } else if (mode == VoiceInputMode.Conversation) {
                                "对话模式：本地检测你是否在说话，确认后才上传识别。"
                            } else {
                                "按住说话"
                            },
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
                itemsIndexed(lines, key = { index, line -> "$index-${line.role}-${line.text.take(24)}" }) { _, line ->
                    Text("${line.role}: ${line.text}", style = MaterialTheme.typography.bodyLarge)
                }
                if (liveUser.isNotBlank() || partial.isNotBlank()) {
                    item {
                        val live = listOf(liveUser, partial).filter { it.isNotBlank() }.joinToString(" ")
                        Text(
                            "user: $live",
                            style = MaterialTheme.typography.bodyLarge,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
                if (busy) {
                    item {
                        Text(
                            "assistant: …",
                            style = MaterialTheme.typography.bodyLarge,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            }
            if (!granted) {
                TextButton(onClick = { launcher.launch(Manifest.permission.RECORD_AUDIO) }) {
                    Text("允许麦克风")
                }
            }
            if (error != null) {
                Text(
                    error ?: "",
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.padding(top = 8.dp),
                )
            }
            if (asr.enabled && granted && mode == VoiceInputMode.Conversation) {
                Text(
                    if (speaking) "正在朗读…" else if (busy) "等待回复…" else listenStatus,
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(top = 8.dp),
                )
                LinearProgressIndicator(
                    progress = if (speaking || busy) 0f else listenLevel,
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(top = 6.dp),
                )
            }
            if (asr.enabled && granted && mode == VoiceInputMode.Hold) {
                Box(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(top = 12.dp)
                        .clip(RoundedCornerShape(12.dp))
                        .background(if (holding) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.primary)
                        .testTag("voice-hold-button")
                        .pointerInput(workspaceId, token, baseUrl) {
                            awaitEachGesture {
                                val down = awaitFirstDown(requireUnconsumed = false)
                                down.consume()
                                if (talk.isHolding()) {
                                    talk.finish(scope) { text ->
                                        liveUser = ""
                                        partial = ""
                                        if (text.isBlank()) {
                                            error = "没听清，请按住再说一次"
                                        } else {
                                            error = null
                                            latestUtterance.value(text)
                                        }
                                    }
                                    waitForUpOrCancellation()
                                    return@awaitEachGesture
                                }
                                error = null
                                liveUser = ""
                                partial = ""
                                latestHoldStart.value()
                                talk.begin(scope, baseUrl, token, workspaceId)
                                try {
                                    waitForUpOrCancellation()
                                } finally {
                                    talk.finish(scope) { text ->
                                        liveUser = ""
                                        partial = ""
                                        if (text.isBlank()) {
                                            error = "没听清，请按住再说一次"
                                        } else {
                                            error = null
                                            latestUtterance.value(text)
                                        }
                                    }
                                }
                            }
                        }
                        .padding(vertical = 18.dp),
                    contentAlignment = Alignment.Center,
                ) {
                    Text(
                        if (holding) "松开结束" else "按住说话",
                        color = MaterialTheme.colorScheme.onPrimary,
                        style = MaterialTheme.typography.titleMedium,
                    )
                }
            }
        }
    }
}

private class HoldToTalkSession(
    private val onHolding: (Boolean) -> Unit,
    private val onPartial: (String) -> Unit,
    private val onFinal: (String) -> Unit,
    private val onError: (String?) -> Unit,
) {
    private var recorderJob: Job? = null
    private var finishJob: Job? = null
    private var socket: WebSocket? = null
    private val holdLines = mutableListOf<String>()
    private var lastPartial = ""
    private val holding = AtomicBoolean(false)
    private val stopping = AtomicBoolean(false)
    private val generation = AtomicInteger(0)
    private var transcriptReady = CompletableDeferred<Unit>()

    fun isHolding(): Boolean = holding.get()

    fun begin(scope: kotlinx.coroutines.CoroutineScope, baseUrl: String, token: String?, workspaceId: String?) {
        if (workspaceId.isNullOrBlank() || token.isNullOrBlank()) {
            onError("先登录并选择 workspace。")
            return
        }
        if (!holding.compareAndSet(false, true)) {
            Log.i(TAG, "begin ignored; already holding")
            return
        }
        val gen = generation.incrementAndGet()
        finishJob?.cancel()
        finishJob = null
        stopping.set(false)
        holdLines.clear()
        lastPartial = ""
        transcriptReady = CompletableDeferred()
        onError(null)
        onPartial("")
        onHolding(true)
        val openedGate = CompletableDeferred<Boolean>()
        val listener = object : WebSocketListener() {
            override fun onOpen(webSocket: WebSocket, response: Response) {
                Log.i(TAG, "asr open gen=$gen")
                openedGate.complete(true)
            }

            override fun onMessage(webSocket: WebSocket, text: String) {
                if (gen != generation.get()) return
                val obj = runCatching { JSONObject(text) }.getOrNull() ?: return
                main {
                    when (obj.optString("type")) {
                        "partial" -> {
                            lastPartial = obj.optString("text")
                            onPartial(lastPartial)
                        }
                        "final" -> {
                            val line = obj.optString("text")
                            if (line.isNotBlank()) holdLines.add(line)
                            lastPartial = ""
                            onFinal(line)
                            onPartial("")
                            transcriptReady.complete(Unit)
                        }
                        "closed" -> {
                            Log.i(TAG, "asr closed gen=$gen stopping=${stopping.get()}")
                            transcriptReady.complete(Unit)
                        }
                        "error" -> {
                            val message = obj.optString("message", "ASR failed")
                            Log.w(TAG, "asr error $message")
                            if (!stopping.get()) onError(message)
                            transcriptReady.complete(Unit)
                        }
                    }
                }
            }

            override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
                Log.w(TAG, "asr onFailure gen=$gen stopping=${stopping.get()} ${t.message}", t)
                openedGate.complete(false)
                transcriptReady.complete(Unit)
                if (gen != generation.get() || stopping.get() || isBenignAsrClose(t)) return
                main {
                    onError(t.message ?: "ASR connection failed")
                    holding.set(false)
                    onHolding(false)
                }
            }

            override fun onClosing(webSocket: WebSocket, code: Int, reason: String) {
                transcriptReady.complete(Unit)
                if (!stopping.get()) {
                    webSocket.close(1000, reason)
                }
            }

            override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
                transcriptReady.complete(Unit)
            }
        }
        val opened = TreerApi().voiceAsrSocket(baseUrl, token, workspaceId, listener)
        socket = opened
        recorderJob = scope.launch(Dispatchers.IO) {
            val connected = withTimeoutOrNull(8_000) { openedGate.await() } == true
            if (!connected || gen != generation.get() || stopping.get()) {
                if (!stopping.get() && gen == generation.get()) {
                    withContext(Dispatchers.Main) {
                        onError("ASR 连接失败")
                        holding.set(false)
                        onHolding(false)
                    }
                }
                return@launch
            }
            val minBuf = AudioRecord.getMinBufferSize(
                16000,
                AudioFormat.CHANNEL_IN_MONO,
                AudioFormat.ENCODING_PCM_16BIT,
            ).coerceAtLeast(3200)
            val record = AudioRecord(
                MediaRecorder.AudioSource.MIC,
                16000,
                AudioFormat.CHANNEL_IN_MONO,
                AudioFormat.ENCODING_PCM_16BIT,
                minBuf,
            )
            if (record.state != AudioRecord.STATE_INITIALIZED) {
                record.release()
                if (gen == generation.get() && !stopping.get()) {
                    withContext(Dispatchers.Main) {
                        onError("无法打开麦克风")
                        holding.set(false)
                        onHolding(false)
                    }
                }
                return@launch
            }
            try {
                record.startRecording()
                val buffer = ByteArray(3200)
                while (isActive && gen == generation.get() && !stopping.get()) {
                    val read = record.read(buffer, 0, buffer.size)
                    if (read > 0) {
                        opened.send(ByteString.of(*buffer.copyOf(read)))
                    }
                }
            } catch (ex: Exception) {
                Log.w(TAG, "recorder failed", ex)
                if (gen == generation.get() && !stopping.get()) {
                    withContext(Dispatchers.Main) {
                        onError(ex.message)
                        holding.set(false)
                        onHolding(false)
                    }
                }
            } finally {
                runCatching { record.stop() }
                record.release()
            }
        }
    }

    fun finish(scope: kotlinx.coroutines.CoroutineScope, onDone: (String) -> Unit) {
        if (!holding.compareAndSet(true, false)) {
            return
        }
        val gen = generation.get()
        stopping.set(true)
        onHolding(false)
        recorderJob?.cancel()
        recorderJob = null
        runCatching { socket?.send("""{"type":"stop"}""") }
        finishJob?.cancel()
        finishJob = scope.launch {
            withTimeoutOrNull(1_800) { transcriptReady.await() }
            if (lastPartial.isNotBlank() && holdLines.none { it.contains(lastPartial) }) {
                kotlinx.coroutines.delay(250)
            }
            if (gen != generation.get()) return@launch
            val text = consumeHold()
            closeSocket()
            withContext(Dispatchers.Main) { onDone(text) }
        }
    }

    fun closeSocket() {
        stopping.set(true)
        socket?.close(1000, "release")
        socket = null
    }

    fun shutdown() {
        generation.incrementAndGet()
        stopping.set(true)
        holding.set(false)
        onHolding(false)
        finishJob?.cancel()
        finishJob = null
        recorderJob?.cancel()
        recorderJob = null
        socket?.close(1000, "shutdown")
        socket = null
        holdLines.clear()
        lastPartial = ""
    }

    fun consumeHold(): String {
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

    private fun main(block: () -> Unit) {
        android.os.Handler(android.os.Looper.getMainLooper()).post(block)
    }

    companion object {
        private const val TAG = "TreerVoice"
    }
}

internal fun isBenignAsrClose(error: Throwable): Boolean {
    val message = error.message.orEmpty().lowercase()
    return message.contains("socket closed") ||
        message.contains("closed") ||
        message.contains("eof") ||
        message.contains("canceled") ||
        message.contains("cancelled")
}
