package ai.treer.mobile.data

import android.content.Context
import android.content.SharedPreferences
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey
import java.io.File
import java.nio.ByteBuffer
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * Session token storage. Prefers EncryptedSharedPreferences; falls back to an
 * Android Keystore AES-GCM file. Never writes the token to plaintext prefs.
 */
class SessionStore(context: Context) {
    private val appContext = context.applicationContext
    private val encrypted: SharedPreferences? = runCatching {
        val masterKey = MasterKey.Builder(appContext)
            .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
            .build()
        EncryptedSharedPreferences.create(
            appContext,
            PREFS_NAME,
            masterKey,
            EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
            EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
        )
    }.getOrNull()
    private val fileStore = KeystoreFileStore(appContext)

    fun token(): String? {
        val fromPrefs = encrypted?.getString(KEY_TOKEN, null)?.takeIf { it.isNotBlank() }
        if (fromPrefs != null) return fromPrefs
        return fileStore.read()
    }

    fun userId(): String? = encrypted?.getString(KEY_USER_ID, null) ?: fileStore.readMeta(KEY_USER_ID)
    fun email(): String? = encrypted?.getString(KEY_EMAIL, null) ?: fileStore.readMeta(KEY_EMAIL)
    fun preferredName(): String? = encrypted?.getString(KEY_NAME, null) ?: fileStore.readMeta(KEY_NAME)

    fun save(token: String, userId: String, email: String, preferredName: String) {
        encrypted?.edit()
            ?.putString(KEY_TOKEN, token)
            ?.putString(KEY_USER_ID, userId)
            ?.putString(KEY_EMAIL, email)
            ?.putString(KEY_NAME, preferredName)
            ?.apply()
        fileStore.write(token, userId, email, preferredName)
    }

    fun clear() {
        encrypted?.edit()?.clear()?.apply()
        fileStore.clear()
    }

    private companion object {
        const val PREFS_NAME = "treer_session_encrypted"
        const val KEY_TOKEN = "token"
        const val KEY_USER_ID = "user_id"
        const val KEY_EMAIL = "email"
        const val KEY_NAME = "preferred_name"
    }
}

private class KeystoreFileStore(private val context: Context) {
    private val file = File(context.noBackupFilesDir, "session.bin")
    private val metaFile = File(context.noBackupFilesDir, "session.meta")

    fun read(): String? {
        if (!file.exists()) return null
        return runCatching {
            val packed = file.readBytes()
            if (packed.size < 12) return null
            val iv = packed.copyOfRange(0, 12)
            val ciphertext = packed.copyOfRange(12, packed.size)
            val cipher = Cipher.getInstance(TRANSFORMATION)
            cipher.init(Cipher.DECRYPT_MODE, secretKey(), GCMParameterSpec(128, iv))
            String(cipher.doFinal(ciphertext), Charsets.UTF_8)
        }.getOrNull()?.takeIf { it.isNotBlank() }
    }

    fun readMeta(key: String): String? {
        if (!metaFile.exists()) return null
        return runCatching {
            metaFile.readLines().firstOrNull { it.startsWith("$key=") }?.substringAfter("=")
        }.getOrNull()
    }

    fun write(token: String, userId: String, email: String, preferredName: String) {
        runCatching {
            val cipher = Cipher.getInstance(TRANSFORMATION)
            cipher.init(Cipher.ENCRYPT_MODE, secretKey())
            val iv = cipher.iv
            val ciphertext = cipher.doFinal(token.toByteArray(Charsets.UTF_8))
            val packed = ByteBuffer.allocate(iv.size + ciphertext.size).put(iv).put(ciphertext).array()
            file.writeBytes(packed)
            metaFile.writeText("user_id=$userId\nemail=$email\npreferred_name=$preferredName\n")
        }
    }

    fun clear() {
        runCatching { file.delete() }
        runCatching { metaFile.delete() }
    }

    private fun secretKey(): SecretKey {
        val keyStore = KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) }
        val existing = keyStore.getKey(KEY_ALIAS, null) as? SecretKey
        if (existing != null) return existing
        val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, ANDROID_KEYSTORE)
        generator.init(
            KeyGenParameterSpec.Builder(
                KEY_ALIAS,
                KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
            )
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .build(),
        )
        return generator.generateKey()
    }

    companion object {
        private const val ANDROID_KEYSTORE = "AndroidKeyStore"
        private const val KEY_ALIAS = "ai.treer.mobile.session"
        private const val TRANSFORMATION = "AES/GCM/NoPadding"
    }
}
