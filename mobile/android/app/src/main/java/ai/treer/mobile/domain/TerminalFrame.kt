package ai.treer.mobile.domain

import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.charset.StandardCharsets

enum class TerminalBinaryKind(val code: Int) {
    Ready(1),
    Output(2),
    Input(3);

    companion object {
        fun from(code: Int): TerminalBinaryKind? = entries.firstOrNull { it.code == code }
    }
}

data class TerminalBinaryFrame(
    val kind: TerminalBinaryKind,
    val sessionId: String,
    val revision: Long,
    val payload: ByteArray,
) {
    fun encode(): ByteArray {
        val session = sessionId.toByteArray(StandardCharsets.UTF_8)
        require(session.isNotEmpty()) { "terminal session id is empty" }
        require(session.size <= 0xFFFF) { "terminal session id is too long" }
        val encoded = ByteArray(HEADER_LEN + session.size + payload.size)
        encoded[0] = VERSION
        encoded[1] = kind.code.toByte()
        encoded[2] = ((session.size ushr 8) and 0xFF).toByte()
        encoded[3] = (session.size and 0xFF).toByte()
        ByteBuffer.wrap(encoded, 4, 8).order(ByteOrder.BIG_ENDIAN).putLong(revision)
        System.arraycopy(session, 0, encoded, HEADER_LEN, session.size)
        System.arraycopy(payload, 0, encoded, HEADER_LEN + session.size, payload.size)
        return encoded
    }

    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is TerminalBinaryFrame) return false
        return kind == other.kind &&
            sessionId == other.sessionId &&
            revision == other.revision &&
            payload.contentEquals(other.payload)
    }

    override fun hashCode(): Int {
        var result = kind.hashCode()
        result = 31 * result + sessionId.hashCode()
        result = 31 * result + revision.hashCode()
        result = 31 * result + payload.contentHashCode()
        return result
    }

    companion object {
        const val VERSION: Byte = 1
        const val HEADER_LEN = 12

        fun decode(encoded: ByteArray): TerminalBinaryFrame {
            if (encoded.size < HEADER_LEN) {
                throw IllegalArgumentException("terminal binary frame is shorter than its header")
            }
            if (encoded[0] != VERSION) {
                throw IllegalArgumentException(
                    "terminal binary frame uses version ${encoded[0].toInt() and 0xFF}, expected ${VERSION.toInt()}",
                )
            }
            val kind = TerminalBinaryKind.from(encoded[1].toInt() and 0xFF)
                ?: throw IllegalArgumentException("unknown terminal binary frame kind ${encoded[1].toInt() and 0xFF}")
            val sessionLen = ((encoded[2].toInt() and 0xFF) shl 8) or (encoded[3].toInt() and 0xFF)
            if (sessionLen == 0) {
                throw IllegalArgumentException("terminal session id is empty")
            }
            val payloadOffset = HEADER_LEN + sessionLen
            if (payloadOffset > encoded.size) {
                throw IllegalArgumentException("terminal session id exceeds the binary frame")
            }
            val revision = ByteBuffer.wrap(encoded, 4, 8).order(ByteOrder.BIG_ENDIAN).long
            val session = String(encoded, HEADER_LEN, sessionLen, StandardCharsets.UTF_8)
            val payload = encoded.copyOfRange(payloadOffset, encoded.size)
            return TerminalBinaryFrame(kind, session, revision, payload)
        }

        fun tryDecode(encoded: ByteArray): TerminalBinaryFrame? {
            return try {
                decode(encoded)
            } catch (_: Exception) {
                null
            }
        }
    }
}

fun stripAnsi(text: String): String {
    val withoutCsi = CSI_REGEX.replace(text, "")
    val withoutOsc = OSC_REGEX.replace(withoutCsi, "")
    return CHARSET_REGEX.replace(withoutOsc, "")
        .replace("\u0007", "")
        .replace("\u0008", "")
        .replace("\u000f", "")
        .replace("\u000e", "")
}

private val CSI_REGEX = Regex("\\u001B\\[[0-?]*[ -/]*[@-~]")
private val OSC_REGEX = Regex("\\u001B\\][^\u0007\\u001B]*(?:\\u0007|\\u001B\\\\)")
private val CHARSET_REGEX = Regex("\\u001B[()][0-9A-Za-z]")
