package ai.treer.mobile.ui.theme

import android.app.Activity
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.SideEffect
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalView
import androidx.core.view.WindowCompat

private val Green = Color(0xFF16A34A)
private val DarkBackground = Color(0xFF0F1215)
private val DarkSurface = Color(0xFF171B1F)
private val DarkOn = Color(0xFFF4F6F7)
private val LightBackground = Color(0xFFF7F8F8)
private val LightSurface = Color(0xFFFFFFFF)
private val LightOn = Color(0xFF111827)

@Composable
fun TreerTheme(theme: String, content: @Composable () -> Unit) {
    val systemDark = isSystemInDarkTheme()
    val dark = when (theme) {
        "light" -> false
        "dark" -> true
        else -> systemDark
    }
    val colors = if (dark) {
        darkColorScheme(
            primary = Green,
            onPrimary = Color.White,
            background = DarkBackground,
            onBackground = DarkOn,
            surface = DarkSurface,
            onSurface = DarkOn,
            surfaceVariant = Color(0xFF22272C),
            onSurfaceVariant = Color(0xFFC5CCD1),
            outline = Color(0xFF3B4750),
            error = Color(0xFFF87171),
        )
    } else {
        lightColorScheme(
            primary = Green,
            onPrimary = Color.White,
            background = LightBackground,
            onBackground = LightOn,
            surface = LightSurface,
            onSurface = LightOn,
            surfaceVariant = Color(0xFFEEF1F3),
            onSurfaceVariant = Color(0xFF4B5563),
            outline = Color(0xFFD1D5DB),
            error = Color(0xFFDC2626),
        )
    }
    val view = LocalView.current
    if (!view.isInEditMode) {
        SideEffect {
            val window = (view.context as Activity).window
            val controller = WindowCompat.getInsetsController(window, view)
            controller.isAppearanceLightStatusBars = !dark
            controller.isAppearanceLightNavigationBars = !dark
        }
    }
    MaterialTheme(colorScheme = colors, content = content)
}
