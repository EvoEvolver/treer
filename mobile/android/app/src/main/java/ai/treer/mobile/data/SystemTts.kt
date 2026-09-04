package ai.treer.mobile.data

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.net.Uri
import android.speech.tts.TextToSpeech
import android.speech.tts.UtteranceProgressListener
import android.util.Log
import ai.treer.mobile.domain.VoiceTtsStatus
import java.util.Locale

class SystemTts(
    context: Context,
    private val onStatus: (VoiceTtsStatus) -> Unit = {},
    private val onSpeaking: (Boolean) -> Unit = {},
) : TextToSpeech.OnInitListener {
    private val app = context.applicationContext
    private val audio = app.getSystemService(Context.AUDIO_SERVICE) as AudioManager
    private var pending: String? = null
    private var ready = false
    private var languageReady = false
    private var engine: TextToSpeech? = null

    init {
        engine = TextToSpeech(app, this)
    }
    private val focusRequest = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN_TRANSIENT_MAY_DUCK)
        .setAudioAttributes(mediaSpeechAttributes())
        .build()

    override fun onInit(status: Int) {
        val tts = engine
        if (status != TextToSpeech.SUCCESS || tts == null) {
            ready = false
            languageReady = false
            pending = null
            val engines = runCatching { tts?.engines?.map { it.name } }.getOrNull()
            Log.w(TAG, "TTS onInit failed status=$status engines=$engines")
            onStatus(diagnose(initFailed = true))
            return
        }
        ready = true
        tts.setAudioAttributes(mediaSpeechAttributes())
        tts.setOnUtteranceProgressListener(object : UtteranceProgressListener() {
            override fun onStart(utteranceId: String?) {
                onSpeaking(true)
            }

            override fun onDone(utteranceId: String?) {
                onSpeaking(false)
                audio.abandonAudioFocusRequest(focusRequest)
            }

            @Deprecated("Deprecated in Java")
            override fun onError(utteranceId: String?) {
                onSpeaking(false)
                audio.abandonAudioFocusRequest(focusRequest)
                onStatus(diagnose(speakFailed = true, detail = "朗读引擎报错"))
            }

            override fun onError(utteranceId: String?, errorCode: Int) {
                onSpeaking(false)
                audio.abandonAudioFocusRequest(focusRequest)
                onStatus(diagnose(speakFailed = true, detail = "朗读失败（$errorCode）"))
            }
        })
        val language = prepareLanguage(null)
        languageReady = language
        onStatus(diagnose(initFailed = false, languageFailed = !language))
        if (language) {
            pending?.let(::speak)
        }
        pending = null
    }

    fun speak(text: String) {
        val trimmed = text.trim()
        if (trimmed.isEmpty()) return
        if (!ready) {
            pending = trimmed
            onStatus(diagnose(initFailed = true))
            return
        }
        if (!prepareLanguage(trimmed)) {
            languageReady = false
            onStatus(diagnose(languageFailed = true))
            return
        }
        languageReady = true
        val muted = audio.getStreamVolume(AudioManager.STREAM_MUSIC) == 0
        if (muted) {
            onStatus(diagnose(volumeMuted = true))
            return
        }
        audio.requestAudioFocus(focusRequest)
        val result = engine?.speak(trimmed, TextToSpeech.QUEUE_FLUSH, null, UTTERANCE_ID) ?: TextToSpeech.ERROR
        if (result == TextToSpeech.ERROR) {
            onSpeaking(false)
            audio.abandonAudioFocusRequest(focusRequest)
            onStatus(diagnose(speakFailed = true, detail = "系统拒绝播放"))
            return
        }
        onStatus(VoiceTtsStatus(ready = true))
    }

    fun stop() {
        pending = null
        onSpeaking(false)
        if (ready) {
            engine?.stop()
        }
        audio.abandonAudioFocusRequest(focusRequest)
    }

    fun shutdown() {
        pending = null
        ready = false
        languageReady = false
        onSpeaking(false)
        engine?.stop()
        engine?.shutdown()
        audio.abandonAudioFocusRequest(focusRequest)
    }

    fun installEngine() {
        openOrMarket("com.google.android.tts")
    }

    fun installVoiceData() {
        val intent = Intent(TextToSpeech.Engine.ACTION_INSTALL_TTS_DATA)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        try {
            app.startActivity(intent)
        } catch (_: ActivityNotFoundException) {
            openTtsSettings()
        }
    }

    fun openTtsSettings() {
        val intent = Intent("com.android.settings.TTS_SETTINGS")
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        try {
            app.startActivity(intent)
        } catch (_: ActivityNotFoundException) {
            val fallback = Intent(android.provider.Settings.ACTION_ACCESSIBILITY_SETTINGS)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            app.startActivity(fallback)
        }
    }

    private fun prepareLanguage(text: String?): Boolean {
        val preferred = if (text == null) {
            listOf(Locale.CHINA, Locale.CHINESE, Locale.US)
        } else {
            listOf(localeFor(text), Locale.CHINA, Locale.CHINESE, Locale.US, Locale.getDefault())
        }
        for (locale in preferred.distinct()) {
            val available = engine?.setLanguage(locale) ?: TextToSpeech.ERROR
            if (available == TextToSpeech.LANG_AVAILABLE ||
                available == TextToSpeech.LANG_COUNTRY_AVAILABLE ||
                available == TextToSpeech.LANG_COUNTRY_VAR_AVAILABLE
            ) {
                return true
            }
        }
        return false
    }

    private fun diagnose(
        initFailed: Boolean = false,
        languageFailed: Boolean = false,
        speakFailed: Boolean = false,
        volumeMuted: Boolean = false,
        detail: String? = null,
    ): VoiceTtsStatus {
        val engines = runCatching { engine?.engines }.getOrNull().orEmpty()
        val noEngine = engines.isEmpty() || initFailed
        val muted = volumeMuted || audio.getStreamVolume(AudioManager.STREAM_MUSIC) == 0
        val message = when {
            noEngine -> "没有可用的系统朗读引擎。请安装「语音识别及语音合成」后再试。"
            languageFailed -> "朗读引擎没有中文/英文语音包。请安装语音数据。"
            muted -> "媒体音量是 0，系统朗读被静音了。请调高音量。"
            speakFailed -> detail ?: "系统朗读失败。"
            else -> null
        }
        return VoiceTtsStatus(
            ready = ready && languageReady && message == null,
            message = message,
            volumeMuted = muted && message != null,
            canInstallEngine = noEngine || initFailed,
            canInstallVoiceData = languageFailed,
            canOpenSettings = message != null,
        )
    }

    private fun openOrMarket(packageName: String) {
        val market = Intent(Intent.ACTION_VIEW, Uri.parse("market://details?id=$packageName"))
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        try {
            app.startActivity(market)
        } catch (_: ActivityNotFoundException) {
            val web = Intent(
                Intent.ACTION_VIEW,
                Uri.parse("https://play.google.com/store/apps/details?id=$packageName"),
            ).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            app.startActivity(web)
        }
    }

    companion object {
        private const val TAG = "TreerVoice"
        private const val UTTERANCE_ID = "treer-voice-reply"

        fun localeFor(text: String): Locale {
            return if (text.any { it in '\u4e00'..'\u9fff' }) Locale.CHINA else Locale.US
        }

        private fun mediaSpeechAttributes(): AudioAttributes {
            return AudioAttributes.Builder()
                .setUsage(AudioAttributes.USAGE_MEDIA)
                .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
                .build()
        }
    }
}
