package org.zerodroid.bridge

/** Pure, JVM-testable policy for the final AccessibilityService trust boundary. */
internal object UiSecurityPolicy {
    data class Rejection(val code: String, val message: String)
    data class VisibleText(val text: String?, val description: String?)
    enum class ClipboardCleanupAction { RESTORE_PREVIOUS, CLEAR_AGENT_CLIP }

    fun visibleText(isPassword: Boolean, text: String?, description: String?): VisibleText =
        if (isPassword) VisibleText(null, null) else VisibleText(text, description)

    fun observationRejection(actual: String?, ownPackage: String): Rejection? = when {
        actual == null -> Rejection("service_unavailable", "No active window package")
        actual == ownPackage -> Rejection(
            "sensitive_target",
            "zerodroid's credential-bearing UI is not observable",
        )
        else -> null
    }

    fun focusRejection(
        expectedPackage: String,
        focusedPackage: String?,
        ownPackage: String,
    ): Rejection? = when {
        focusedPackage == null -> Rejection("no_focus", "No input-focused field")
        focusedPackage == ownPackage -> Rejection(
            "no_focus",
            "Input focus is on zerodroid's own overlay, not the target app. Tap the field you " +
                "want to type into first.",
        )
        focusedPackage != expectedPackage -> Rejection(
            "wrong_foreground",
            "Expected $expectedPackage but input focus belongs to $focusedPackage",
        )
        else -> null
    }

    fun clipboardCleanupAction(previousClipReadable: Boolean): ClipboardCleanupAction =
        if (previousClipReadable) ClipboardCleanupAction.RESTORE_PREVIOUS
        else ClipboardCleanupAction.CLEAR_AGENT_CLIP

    fun launchRejection(
        requestedPackage: String,
        ownPackage: String,
        explicitActivity: String?,
        resolvedActivityExported: Boolean?,
    ): Rejection? = when {
        requestedPackage == ownPackage -> Rejection(
            "sensitive_target",
            "zerodroid cannot launch its own credential-bearing UI",
        )
        explicitActivity != null && resolvedActivityExported == null -> Rejection(
            "not_found",
            "activity $explicitActivity was not found in $requestedPackage",
        )
        explicitActivity != null && resolvedActivityExported == false -> Rejection(
            "not_found",
            "activity $explicitActivity is not exported",
        )
        else -> null
    }

    fun screenshotCallbackRejection(
        initialPackage: String,
        callbackPackage: String?,
        ownPackage: String,
    ): Rejection? {
        observationRejection(callbackPackage, ownPackage)?.let { return it }
        return if (callbackPackage != initialPackage) {
            Rejection(
                "wrong_foreground",
                "Foreground changed from $initialPackage to $callbackPackage during screenshot",
            )
        } else {
            null
        }
    }

    fun mutationRejection(
        expected: String?,
        actual: String?,
        ownPackage: String,
        systemDialogPackages: Set<String>,
        privilegedDialog: Boolean,
    ): Rejection? = when {
        expected.isNullOrBlank() -> Rejection("bad_args", "expect_package is required")
        actual == null -> Rejection("service_unavailable", "No active window")
        actual != expected -> Rejection(
            "wrong_foreground",
            "Expected $expected but $actual is foreground",
        )
        actual == ownPackage -> Rejection(
            "sensitive_target",
            "zerodroid cannot drive its own credential-bearing UI",
        )
        privilegedDialog && actual !in systemDialogPackages -> Rejection(
            "not_found",
            "No recognized system dialog showing (foreground: $actual)",
        )
        !privilegedDialog && actual in systemDialogPackages -> Rejection(
            "manual_confirmation_required",
            "Ordinary UI actions cannot operate a privileged system dialog; use android_dialog",
        )
        else -> null
    }
}
