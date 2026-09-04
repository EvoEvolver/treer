package ai.treer.mobile

import ai.treer.mobile.data.LocalSpeechGate
import ai.treer.mobile.data.SpeechTurn
import ai.treer.mobile.data.SpeechTurnDetector
import ai.treer.mobile.data.SystemTts
import ai.treer.mobile.data.parseVoiceAsrStatus
import ai.treer.mobile.data.parseVoiceCommandReply
import ai.treer.mobile.data.parseVoiceCommandStatus
import com.google.gson.JsonParser
import java.util.Locale
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class VoiceAsrParseTest {
    @Test
    fun parsesEnabledQwenStatus() {
        val json = JsonParser.parseString(
            """{"enabled":true,"provider":"qwen","sample_rate":16000,"encoding":"pcm16"}""",
        ).asJsonObject
        val status = parseVoiceAsrStatus(json)
        assertTrue(status.enabled)
        assertEquals("qwen", status.provider)
        assertEquals(16000, status.sampleRate)
        assertEquals("pcm16", status.encoding)
    }

    @Test
    fun parsesDisabledStatus() {
        val json = JsonParser.parseString("""{"enabled":false,"provider":null,"sample_rate":16000,"encoding":"pcm16"}""").asJsonObject
        val status = parseVoiceAsrStatus(json)
        assertFalse(status.enabled)
        assertEquals(null, status.provider)
    }

    @Test
    fun parsesVoiceCommandStatusAndReply() {
        val status = parseVoiceCommandStatus(
            JsonParser.parseString(
                """{"enabled":true,"wire_api":"responses","model":"gpt-5.6-luna"}""",
            ).asJsonObject,
        )
        assertTrue(status.enabled)
        assertEquals("responses", status.wireApi)
        assertEquals("gpt-5.6-luna", status.model)
        val reply = parseVoiceCommandReply(
            JsonParser.parseString(
                """{"reply":"已经发给 reviewer 了。","utterance":"让 reviewer 写测试","tools":[{"ok":true}]}""",
            ).asJsonObject,
        )
        assertEquals("已经发给 reviewer 了。", reply.reply)
        assertEquals("让 reviewer 写测试", reply.utterance)
    }

    @Test
    fun treatsSocketClosedAsBenignAsrHangup() {
        assertTrue(ai.treer.mobile.ui.components.isBenignAsrClose(java.io.IOException("Socket closed")))
        assertFalse(ai.treer.mobile.ui.components.isBenignAsrClose(java.io.IOException("401 unauthorized")))
    }

    @Test
    fun picksChineseTtsLocaleForCjkReply() {
        assertEquals(Locale.CHINA, SystemTts.localeFor("已经发给 reviewer 了。"))
        assertEquals(Locale.US, SystemTts.localeFor("Prompt sent to reviewer."))
    }

    @Test
    fun speechTurnDetectorIgnoresCoughLengthBursts() {
        val detector = SpeechTurnDetector(minSpeechMs = 220, minSilenceMs = 900, frameMs = 32)
        repeat(4) { assertEquals(null, detector.onFrame(true)) }
        assertEquals(null, detector.onFrame(false))
        assertEquals(false, detector.inSpeech)
        var started = false
        repeat(20) {
            if (detector.onFrame(true) == SpeechTurn.Started) started = true
        }
        assertTrue(started)
        var ended = false
        repeat(40) {
            if (detector.onFrame(false) == SpeechTurn.Ended) ended = true
        }
        assertTrue(ended)
    }

    @Test
    fun localSpeechGateRejectsSilence() {
        val gate = LocalSpeechGate()
        assertFalse(gate.isSpeech(ByteArray(1024)))
    }
}
