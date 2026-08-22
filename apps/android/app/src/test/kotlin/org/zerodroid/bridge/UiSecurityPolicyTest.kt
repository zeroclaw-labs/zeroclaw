package org.zerodroid.bridge

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class UiSecurityPolicyTest {
    private val dialogs = setOf("com.android.permissioncontroller", "com.android.systemui")

    @Test
    fun passwordNodesNeverExposeTextOrDescription() {
        val visible = UiSecurityPolicy.visibleText(true, "hunter2", "password hunter2")
        assertNull(visible.text)
        assertNull(visible.description)
    }

    @Test
    fun normalNodesKeepAccessibilityLabels() {
        val visible = UiSecurityPolicy.visibleText(false, "Save", "Save changes")
        assertEquals("Save", visible.text)
        assertEquals("Save changes", visible.description)
    }

    @Test
    fun observationFailsClosedWithoutAPackageAndRejectsOwnUi() {
        assertEquals(
            "service_unavailable",
            UiSecurityPolicy.observationRejection(null, "org.zerodroid.bridge")?.code,
        )
        assertEquals(
            "sensitive_target",
            UiSecurityPolicy.observationRejection(
                "org.zerodroid.bridge",
                "org.zerodroid.bridge",
            )?.code,
        )
        assertNull(
            UiSecurityPolicy.observationRejection(
                "com.example.target",
                "org.zerodroid.bridge",
            )
        )
    }

    @Test
    fun focusGuardRejectsMissingFocus() {
        assertEquals(
            "no_focus",
            UiSecurityPolicy.focusRejection(
                "com.example.target",
                null,
                "org.zerodroid.bridge",
            )?.code,
        )
    }

    @Test
    fun focusGuardRejectsOwnOverlayWithHelpfulRecovery() {
        val rejection = UiSecurityPolicy.focusRejection(
            "com.example.target",
            "org.zerodroid.bridge",
            "org.zerodroid.bridge",
        )

        assertEquals("no_focus", rejection?.code)
        assertTrue(rejection?.message?.contains("Tap the field") == true)
    }

    @Test
    fun focusGuardRejectsFocusFromAnotherPackage() {
        assertEquals(
            "wrong_foreground",
            UiSecurityPolicy.focusRejection(
                "com.example.target",
                "com.example.other",
                "org.zerodroid.bridge",
            )?.code,
        )
    }

    @Test
    fun focusGuardAcceptsExpectedPackage() {
        assertNull(
            UiSecurityPolicy.focusRejection(
                "com.example.target",
                "com.example.target",
                "org.zerodroid.bridge",
            )
        )
    }

    @Test
    fun clipboardCleanupRestoresAReadablePreviousClip() {
        assertEquals(
            UiSecurityPolicy.ClipboardCleanupAction.RESTORE_PREVIOUS,
            UiSecurityPolicy.clipboardCleanupAction(previousClipReadable = true),
        )
    }

    @Test
    fun clipboardCleanupClearsAgentTextWhenPreviousClipWasUnreadable() {
        assertEquals(
            UiSecurityPolicy.ClipboardCleanupAction.CLEAR_AGENT_CLIP,
            UiSecurityPolicy.clipboardCleanupAction(previousClipReadable = false),
        )
    }

    @Test
    fun launchGuardRejectsOwnPackage() {
        assertEquals(
            "sensitive_target",
            UiSecurityPolicy.launchRejection(
                requestedPackage = "org.zerodroid.bridge",
                ownPackage = "org.zerodroid.bridge",
                explicitActivity = null,
                resolvedActivityExported = null,
            )?.code,
        )
    }

    @Test
    fun launchGuardRejectsMissingExplicitActivity() {
        assertEquals(
            "not_found",
            UiSecurityPolicy.launchRejection(
                requestedPackage = "com.example.target",
                ownPackage = "org.zerodroid.bridge",
                explicitActivity = "com.example.target.MissingActivity",
                resolvedActivityExported = null,
            )?.code,
        )
    }

    @Test
    fun launchGuardRejectsUnexportedExplicitActivity() {
        assertEquals(
            "not_found",
            UiSecurityPolicy.launchRejection(
                requestedPackage = "com.example.target",
                ownPackage = "org.zerodroid.bridge",
                explicitActivity = "com.example.target.PrivateActivity",
                resolvedActivityExported = false,
            )?.code,
        )
    }

    @Test
    fun launchGuardAcceptsOtherPackagesAndExportedExplicitActivities() {
        assertNull(
            UiSecurityPolicy.launchRejection(
                requestedPackage = "com.example.target",
                ownPackage = "org.zerodroid.bridge",
                explicitActivity = null,
                resolvedActivityExported = null,
            )
        )
        assertNull(
            UiSecurityPolicy.launchRejection(
                requestedPackage = "com.example.target",
                ownPackage = "org.zerodroid.bridge",
                explicitActivity = "com.example.target.PublicActivity",
                resolvedActivityExported = true,
            )
        )
    }

    @Test
    fun screenshotCallbackRejectsMissingForegroundPackage() {
        assertEquals(
            "service_unavailable",
            UiSecurityPolicy.screenshotCallbackRejection(
                initialPackage = "com.example.target",
                callbackPackage = null,
                ownPackage = "org.zerodroid.bridge",
            )?.code,
        )
    }

    @Test
    fun screenshotCallbackRejectsOwnForegroundPackage() {
        assertEquals(
            "sensitive_target",
            UiSecurityPolicy.screenshotCallbackRejection(
                initialPackage = "com.example.target",
                callbackPackage = "org.zerodroid.bridge",
                ownPackage = "org.zerodroid.bridge",
            )?.code,
        )
    }

    @Test
    fun screenshotCallbackRejectsAChangedForegroundPackage() {
        assertEquals(
            "wrong_foreground",
            UiSecurityPolicy.screenshotCallbackRejection(
                initialPackage = "com.example.target",
                callbackPackage = "com.example.other",
                ownPackage = "org.zerodroid.bridge",
            )?.code,
        )
    }

    @Test
    fun screenshotCallbackAcceptsTheOriginalForegroundPackage() {
        assertNull(
            UiSecurityPolicy.screenshotCallbackRejection(
                initialPackage = "com.example.target",
                callbackPackage = "com.example.target",
                ownPackage = "org.zerodroid.bridge",
            )
        )
    }

    @Test
    fun mutationRequiresExactForegroundAndRejectsOwnUi() {
        assertEquals(
            "bad_args",
            UiSecurityPolicy.mutationRejection(null, "com.example", "org.zerodroid.bridge", dialogs, false)?.code,
        )
        assertEquals(
            "wrong_foreground",
            UiSecurityPolicy.mutationRejection("com.target", "com.other", "org.zerodroid.bridge", dialogs, false)?.code,
        )
        assertEquals(
            "sensitive_target",
            UiSecurityPolicy.mutationRejection(
                "org.zerodroid.bridge",
                "org.zerodroid.bridge",
                "org.zerodroid.bridge",
                dialogs,
                false,
            )?.code,
        )
    }

    @Test
    fun systemDialogsRequireThePrivilegedPath() {
        assertEquals(
            "manual_confirmation_required",
            UiSecurityPolicy.mutationRejection(
                "com.android.permissioncontroller",
                "com.android.permissioncontroller",
                "org.zerodroid.bridge",
                dialogs,
                false,
            )?.code,
        )
        assertNull(
            UiSecurityPolicy.mutationRejection(
                "com.android.permissioncontroller",
                "com.android.permissioncontroller",
                "org.zerodroid.bridge",
                dialogs,
                true,
            )
        )
    }
}
