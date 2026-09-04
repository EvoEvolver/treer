package ai.treer.mobile.data

import kotlin.math.sqrt

data class SpeechGateResult(
    val speech: Boolean,
    val rms: Double,
)

/**
 * Local speech-likeness gate: adaptive RMS, with ZCR only rejecting hiss.
 * Consecutive-frame timing (cough vs real speech) belongs in [SpeechTurnDetector].
 */
class LocalSpeechGate {
    private var noise = 300.0

    fun inspect(frame: ByteArray): SpeechGateResult {
        var energy = 0.0
        var crossings = 0
        var samples = 0
        var prev = 0
        var i = 0
        while (i + 1 < frame.size) {
            val sample = (frame[i].toInt() and 0xff) or ((frame[i + 1].toInt() and 0xff) shl 8)
            val value = sample.toShort().toInt()
            energy += value.toDouble() * value
            if (samples > 0 && (prev xor value) < 0 && kotlin.math.abs(value) > 120) {
                crossings += 1
            }
            prev = value
            samples += 1
            i += 2
        }
        if (samples == 0) return SpeechGateResult(false, 0.0)
        val rms = sqrt(energy / samples)
        val zcr = crossings.toDouble() / samples
        if (rms < noise * 1.6) {
            noise = (noise * 0.98) + (rms * 0.02)
            if (noise < 80) noise = 80.0
        }
        val loud = rms > 250 && rms > noise * 1.6
        val notHiss = zcr < 0.45
        return SpeechGateResult(speech = loud && notHiss, rms = rms)
    }

    fun isSpeech(frame: ByteArray): Boolean = inspect(frame).speech
}
