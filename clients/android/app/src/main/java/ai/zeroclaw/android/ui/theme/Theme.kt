package ai.zeroclaw.android.ui.theme

import android.app.Activity
import android.os.Build
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.runtime.SideEffect
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalView
import androidx.core.view.WindowCompat

// ZeroClaw brand colors
private val ZeroClawOrange = Color(0xFFE85C0D)
private val ZeroClawDark = Color(0xFF1A1A2E)

private val DarkColorScheme = darkColorScheme(
    primary = ZeroClawOrange,
    onPrimary = Color.White,
    primaryContainer = Color(0xFF3D2014),
    onPrimaryContainer = Color(0xFFFFDBCA),
    secondary = Color(0xFF8ECAE6),
    onSecondary = Color.Black,
    background = ZeroClawDark,
    surface = Color(0xFF1E1E32),
    surfaceVariant = Color(0xFF2A2A40),
    onBackground = Color.White,
    onSurface = Color.White,
)

private val LightColorScheme = lightColorScheme(
    primary = ZeroClawOrange,
    onPrimary = Color.White,
    primaryContainer = Color(0xFFFFDBCA),
    onPrimaryContainer = Color(0xFF3D2014),
    secondary = Color(0xFF023047),
    onSecondary = Color.White,
    background = Color(0xFFFFFBFE),
    surface = Color(0xFFFFFBFE),
    surfaceVariant = Color(0xFFF5F5F5),
    onBackground = Color(0xFF1C1B1F),
    onSurface = Color(0xFF1C1B1F),
)

@Composable
fun ZeroClawTheme(
    darkTheme: Boolean = isSystemInDarkTheme(),
    dynamicColor: Boolean = true,
    content: @Composable () -> Unit
) {
    val colorScheme = when {
        dynamicColor && Build.VERSION.SDK_INT >= Build.VERSION_CODES.S -> {
            val context = LocalContext.current
            if (darkTheme) dynamicDarkColorScheme(context) else dynamicLightColorScheme(context)
        }
        darkTheme -> DarkColorScheme
        else -> LightColorScheme
    }

    val view = LocalView.current
    if (!view.isInEditMode) {
        SideEffect {
            val window = (view.context as Activity).window
            window.statusBarColor = colorScheme.background.toArgb()
            WindowCompat.getInsetsController(window, view).isAppearanceLightStatusBars = !darkTheme
        }
    }

    MaterialTheme(
        colorScheme = colorScheme,
        typography = Typography,
        content = content
    )
}

val Typography = Typography()
