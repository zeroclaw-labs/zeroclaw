package org.zerodroid.bridge

/** Pure, JVM-testable policy for the final AccessibilityService trust boundary. */
internal object UiSecurityPolicy {
    data class Rejection(val code: String, val message: String)
    data class VisibleText(val text: String?, val description: String?)

    fun visibleText(isPassword: Boolean, text: String?, description: String?): VisibleText =
        if (isPassword) VisibleText(null, null) else VisibleText(text, description)

    fun observationRejection(actual: String?, ownPackage: String): Rejection? = when {
        actual == ownPackage -> Rejection(
            "sensitive_target",
            "zerodroid's credential-bearing UI is not observable",
        )
        else -> null
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
