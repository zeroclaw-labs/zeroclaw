package org.zerodroid.bridge

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
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
