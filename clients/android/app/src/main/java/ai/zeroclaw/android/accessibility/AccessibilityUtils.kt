package ai.zeroclaw.android.accessibility

import android.content.Context
import android.view.accessibility.AccessibilityManager
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.SemanticsPropertyKey
import androidx.compose.ui.semantics.SemanticsPropertyReceiver

/**
 * Accessibility utilities for ZeroClaw Android.
 *
 * Ensures the app is usable with:
 * - TalkBack (screen reader)
 * - Switch Access
 * - Voice Access
 * - Large text/display size
 */
object AccessibilityUtils {

    /**
     * Check if TalkBack or similar screen reader is enabled
     */
    fun isScreenReaderEnabled(context: Context): Boolean {
        val am = context.getSystemService(Context.ACCESSIBILITY_SERVICE) as AccessibilityManager
        return am.isEnabled && am.isTouchExplorationEnabled
    }

    /**
     * Check if any accessibility service is enabled
     */
    fun isAccessibilityEnabled(context: Context): Boolean {
        val am = context.getSystemService(Context.ACCESSIBILITY_SERVICE) as AccessibilityManager
        return am.isEnabled
    }

    /**
     * Get appropriate content description for agent status
     */
    fun getStatusDescription(isRunning: Boolean, isThinking: Boolean = false): String {
        return when {
            isThinking -> "Agent is thinking and processing your request"
            isRunning -> "Agent is running and ready to help"
            else -> "Agent is stopped. Tap to start"
        }
    }

    /**
     * Get content description for chat messages
     */
    fun getMessageDescription(
        content: String,
        isUser: Boolean,
        timestamp: String
    ): String {
        val sender = if (isUser) "You said" else "Agent replied"
        return "$sender at $timestamp: $content"
    }

    /**
     * Announce message for screen readers
     */
    fun announceForAccessibility(context: Context, message: String) {
        val am = context.getSystemService(Context.ACCESSIBILITY_SERVICE) as AccessibilityManager
        if (am.isEnabled) {
            val event = android.view.accessibility.AccessibilityEvent.obtain(
                android.view.accessibility.AccessibilityEvent.TYPE_ANNOUNCEMENT
            )
            event.text.add(message)
            am.sendAccessibilityEvent(event)
        }
    }
}

/**
 * Custom semantic property for live regions
 */
val LiveRegion = SemanticsPropertyKey<LiveRegionMode>("LiveRegion")
var SemanticsPropertyReceiver.liveRegion by LiveRegion

enum class LiveRegionMode {
    None,
    Polite,    // Announce when user is idle
    Assertive  // Announce immediately
}

/**
 * Composable to check screen reader status
 */
@Composable
fun rememberAccessibilityState(): AccessibilityState {
    val context = LocalContext.current
    return remember {
        AccessibilityState(
            isScreenReaderEnabled = AccessibilityUtils.isScreenReaderEnabled(context),
            isAccessibilityEnabled = AccessibilityUtils.isAccessibilityEnabled(context)
        )
    }
}

data class AccessibilityState(
    val isScreenReaderEnabled: Boolean,
    val isAccessibilityEnabled: Boolean
)

/**
 * Content descriptions for common UI elements
 */
object ContentDescriptions {
    const val TOGGLE_AGENT = "Toggle agent on or off"
    const val SEND_MESSAGE = "Send message"
    const val CLEAR_CHAT = "Clear conversation"
    const val OPEN_SETTINGS = "Open settings"
    const val BACK = "Go back"
    const val AGENT_STATUS = "Agent status"
    const val MESSAGE_INPUT = "Type your message here"
    const val PROVIDER_DROPDOWN = "Select AI provider"
    const val MODEL_DROPDOWN = "Select AI model"
    const val API_KEY_INPUT = "Enter your API key"
    const val SHOW_API_KEY = "Show API key"
    const val HIDE_API_KEY = "Hide API key"
}
