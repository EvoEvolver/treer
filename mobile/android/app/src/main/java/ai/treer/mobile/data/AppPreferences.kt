package ai.treer.mobile.data

import android.content.Context
import android.os.Build
import java.util.UUID

class AppPreferences(context: Context) {
    private val prefs = context.applicationContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    var proxyUrl: String
        get() = prefs.getString(KEY_PROXY_URL, "") ?: ""
        set(value) {
            prefs.edit().putString(KEY_PROXY_URL, value.trim().trimEnd('/')).apply()
        }

    var lastOrganizationId: String?
        get() = prefs.getString(KEY_ORG, null)
        set(value) {
            prefs.edit().putString(KEY_ORG, value).apply()
        }

    var lastWorkspaceId: String?
        get() = prefs.getString(KEY_WORKSPACE, null)
        set(value) {
            prefs.edit().putString(KEY_WORKSPACE, value).apply()
        }

    var lastTab: String
        get() = prefs.getString(KEY_TAB, "home") ?: "home"
        set(value) {
            prefs.edit().putString(KEY_TAB, value).apply()
        }

    var theme: String
        get() = prefs.getString(KEY_THEME, "system") ?: "system"
        set(value) {
            prefs.edit().putString(KEY_THEME, value).apply()
        }

    var showTerminalControls: Boolean
        get() = prefs.getBoolean(KEY_TERMINAL, false)
        set(value) {
            prefs.edit().putBoolean(KEY_TERMINAL, value).apply()
        }

    val deviceId: String
        get() {
            val existing = prefs.getString(KEY_DEVICE_ID, null)
            if (!existing.isNullOrBlank()) return existing
            val created = UUID.randomUUID().toString()
            prefs.edit().putString(KEY_DEVICE_ID, created).apply()
            return created
        }

    val deviceName: String
        get() {
            val manufacturer = Build.MANUFACTURER.orEmpty()
            val model = Build.MODEL.orEmpty()
            val name = listOf(manufacturer, model).filter { it.isNotBlank() }.joinToString(" ").ifBlank { "Android" }
            return "Treer Android · $name"
        }

    fun clearWorkspaceMemory() {
        prefs.edit()
            .remove(KEY_ORG)
            .remove(KEY_WORKSPACE)
            .apply()
    }

    fun clearProxy() {
        prefs.edit()
            .remove(KEY_PROXY_URL)
            .remove(KEY_ORG)
            .remove(KEY_WORKSPACE)
            .apply()
    }

    private companion object {
        const val PREFS_NAME = "treer_prefs"
        const val KEY_PROXY_URL = "proxy_url"
        const val KEY_ORG = "last_org"
        const val KEY_WORKSPACE = "last_workspace"
        const val KEY_TAB = "last_tab"
        const val KEY_THEME = "theme"
        const val KEY_TERMINAL = "show_terminal"
        const val KEY_DEVICE_ID = "device_id"
    }
}
