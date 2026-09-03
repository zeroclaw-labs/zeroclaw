package org.zerodroid.bridge

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File

/**
 * JVM regression for Android-only wiring that cannot execute against framework stubs on the host.
 * Pure decision behavior lives in [UiSecurityPolicyTest]; these checks keep those decisions wired
 * at the privileged framework boundary rather than becoming unused, decorative helpers.
 */
class AndroidSecurityWiringTest {
    @Test
    fun focusedNodePolicyGuardsBothTextAndEnter() {
        val source = mainSource("kotlin/org/zerodroid/bridge/UiAccessibilityService.kt")

        assertEquals(2, source.countOccurrences("UiSecurityPolicy.focusRejection("))
        assertTrue(source.contains("handleType(intent.getStringExtra(\"text\") ?: \"\", expectedPackage)"))
        assertTrue(source.contains("handleKey(intent.getStringExtra(\"key\") ?: \"\", expectedPackage)"))
    }

    @Test
    fun clipboardFallbackCleansImmediatelyAfterSynchronousPaste() {
        val source = mainSource("kotlin/org/zerodroid/bridge/UiAccessibilityService.kt")
        val clipboard = source.substring(
            source.indexOf("private fun pasteViaClipboard"),
            source.indexOf("// ── key", source.indexOf("private fun pasteViaClipboard")),
        )

        assertOrdered(clipboard, "cm.setPrimaryClip(agentClip)", "AccessibilityNodeInfo.ACTION_FOCUS")
        assertOrdered(clipboard, "AccessibilityNodeInfo.ACTION_FOCUS", "AccessibilityNodeInfo.ACTION_PASTE")
        assertOrdered(clipboard, "AccessibilityNodeInfo.ACTION_PASTE", "UiSecurityPolicy.clipboardCleanupAction(")
        assertTrue(clipboard.contains("ClipDescription.EXTRA_IS_SENSITIVE"))
        assertTrue(clipboard.contains("cm.clearPrimaryClip()"))
        assertTrue(clipboard.contains("finally"))
        assertFalse(clipboard.contains("postDelayed"))
        assertFalse(clipboard.contains("OnPrimaryClipChangedListener"))
        assertFalse(source.contains("CLIPBOARD_RESTORE_MS"))
    }

    @Test
    fun explicitLaunchChecksResolvedExportedActivityBeforeStarting() {
        val source = mainSource("kotlin/org/zerodroid/bridge/UiSocketServer.kt")

        assertTrue(source.contains("resolvedActivity?.exported"))
        assertOrdered(source, "UiSecurityPolicy.launchRejection(", "ctx.startActivity(intent)")
    }

    @Test
    fun screenshotCallbackRevalidatesBeforeReadingPixels() {
        val source = mainSource("kotlin/org/zerodroid/bridge/UiAccessibilityService.kt")
        val callback = source.indexOf("override fun onSuccess(result: ScreenshotResult)")
        val revalidation = source.indexOf("UiSecurityPolicy.screenshotCallbackRejection(", callback)
        val pixelRead = source.indexOf("Bitmap.wrapHardwareBuffer", callback)

        assertTrue("screenshot success callback is missing", callback >= 0)
        assertTrue("callback foreground revalidation is missing", revalidation > callback)
        assertTrue("callback must revalidate before reading pixels", pixelRead > revalidation)
    }

    @Test
    fun observationsKeepPackageNullableUntilPolicyAdmission() {
        val source = mainSource("kotlin/org/zerodroid/bridge/UiAccessibilityService.kt")
        val readStart = source.indexOf("private fun readScreen")
        val readEnd = source.indexOf("private fun traverseNode", readStart)
        val read = source.substring(readStart, readEnd)
        val screenshotStart = source.indexOf("private fun handleScreenshot")
        val screenshotEnd = source.indexOf("private fun uniformFraction", screenshotStart)
        val screenshot = source.substring(screenshotStart, screenshotEnd)

        assertTrue(read.contains("val pkg = root?.packageName?.toString()"))
        assertFalse(read.contains("?: \"unknown\""))
        assertOrdered(read, "UiSecurityPolicy.observationRejection(", "traverseNode(activeRoot")
        assertOrdered(screenshot, "UiSecurityPolicy.observationRejection(", "takeScreenshot(")
    }

    @Test
    fun signInWindowIsSecureBeforeActivityCreationAndExternalUi() {
        val source = mainSource("kotlin/org/zerodroid/bridge/SignInActivity.kt")
        assertOrdered(
            source,
            "window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)",
            "super.onCreate(savedInstanceState)",
        )
        assertOrdered(
            source,
            "window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)",
            "startActivityForResult(",
        )
    }

    private fun assertOrdered(source: String, first: String, second: String) {
        val firstIndex = source.indexOf(first)
        val secondIndex = source.indexOf(second)
        assertTrue("missing '$first'", firstIndex >= 0)
        assertTrue("missing '$second'", secondIndex >= 0)
        assertTrue("'$first' must appear before '$second'", firstIndex < secondIndex)
    }

    private fun String.countOccurrences(needle: String): Int =
        windowed(needle.length).count { it == needle }

    private fun mainSource(relativePath: String): String {
        val roots = listOf(
            File("src/main"),
            File("app/src/main"),
            File("apps/android/app/src/main"),
        )
        val file = roots.asSequence()
            .map { it.resolve(relativePath) }
            .firstOrNull(File::isFile)
            ?: throw AssertionError("cannot locate Android main source file: $relativePath")
        return file.readText()
    }
}
