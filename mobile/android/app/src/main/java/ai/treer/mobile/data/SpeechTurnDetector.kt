package ai.treer.mobile.data

enum class SpeechTurn {
    Started,
    Ended,
}

/**
 * Hysteresis around a frame-level speech flag.
 * Short bursts (cough, click, chair scrape) never reach [minSpeechMs].
 * End-of-turn waits for [minSilenceMs] so a breath pause does not cut the sentence.
 */
class SpeechTurnDetector(
    private val minSpeechMs: Int = 220,
    private val minSilenceMs: Int = 900,
    private val frameMs: Int = 32,
) {
    private var speechMs = 0
    private var silenceMs = 0
    var inSpeech: Boolean = false
        private set

    fun onFrame(speech: Boolean): SpeechTurn? {
        if (speech) {
            speechMs += frameMs
            silenceMs = 0
            if (!inSpeech && speechMs >= minSpeechMs) {
                inSpeech = true
                return SpeechTurn.Started
            }
        } else if (inSpeech) {
            silenceMs += frameMs
            if (silenceMs >= minSilenceMs) {
                inSpeech = false
                speechMs = 0
                silenceMs = 0
                return SpeechTurn.Ended
            }
        } else {
            speechMs = (speechMs - frameMs).coerceAtLeast(0)
        }
        return null
    }

    fun reset() {
        speechMs = 0
        silenceMs = 0
        inSpeech = false
    }
}
