package ai.treer.mobile

import ai.treer.mobile.domain.TerminalBinaryFrame
import ai.treer.mobile.domain.TerminalBinaryKind
import ai.treer.mobile.domain.stripAnsi
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class TerminalFrameTest {
    @Test
    fun roundTripsOutputFrame() {
        val frame = TerminalBinaryFrame(
            kind = TerminalBinaryKind.Output,
            sessionId = "term_abc",
            revision = 42,
            payload = byteArrayOf(0, 1, 2, 0xff.toByte()),
        )
        val encoded = frame.encode()
        assertEquals(1, encoded[0].toInt())
        assertEquals(2, encoded[1].toInt())
        assertEquals(frame, TerminalBinaryFrame.decode(encoded))
    }

    @Test
    fun encodesReadyAndInputKinds() {
        val ready = TerminalBinaryFrame(TerminalBinaryKind.Ready, "s", 0, byteArrayOf()).encode()
        val input = TerminalBinaryFrame(TerminalBinaryKind.Input, "s", 0, "hi".toByteArray()).encode()
        assertEquals(1, ready[1].toInt())
        assertEquals(3, input[1].toInt())
    }

    @Test
    fun tryDecodeRejectsUnknownVersion() {
        val encoded = TerminalBinaryFrame(
            TerminalBinaryKind.Input,
            "term_abc",
            0,
            "hello".toByteArray(),
        ).encode()
        encoded[0] = 9
        assertNull(TerminalBinaryFrame.tryDecode(encoded))
    }

    @Test
    fun stripsAnsiSequences() {
        val raw = "\u001B[31mred\u001B[0m plain"
        assertEquals("red plain", stripAnsi(raw))
    }
}
