package org.zerodroid.bridge

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class ScreenshotSizingTest {
    @Test
    fun oversizedPayloadChoosesStrictlySmallerWidth() {
        val next = ScreenshotSizing.nextWidth(
            requestedWidth = 540,
            actualWidth = 540,
            encodedBytes = 4 * 1024 * 1024,
            maxEncodedBytes = 2 * 1024 * 1024,
        )

        assertTrue(next != null && next in ScreenshotSizing.MIN_WIDTH until 540)
    }

    @Test
    fun actualWidthIsTheReductionBaseline() {
        val next = ScreenshotSizing.nextWidth(
            requestedWidth = 1080,
            actualWidth = 480,
            encodedBytes = 3 * 1024 * 1024,
            maxEncodedBytes = 2 * 1024 * 1024,
        )

        assertTrue(next != null && next < 480)
    }

    @Test
    fun reductionNeverDropsBelowSupportedMinimum() {
        val next = ScreenshotSizing.nextWidth(
            requestedWidth = 65,
            actualWidth = 65,
            encodedBytes = 200 * 1024 * 1024,
            maxEncodedBytes = 2 * 1024 * 1024,
        )

        assertEquals(ScreenshotSizing.MIN_WIDTH, next)
    }

    @Test
    fun minimumWidthCannotRetry() {
        val next = ScreenshotSizing.nextWidth(
            requestedWidth = ScreenshotSizing.MIN_WIDTH,
            actualWidth = ScreenshotSizing.MIN_WIDTH,
            encodedBytes = 3 * 1024 * 1024,
            maxEncodedBytes = 2 * 1024 * 1024,
        )

        assertNull(next)
    }

    @Test
    fun fittingPayloadNeedsNoRetry() {
        val next = ScreenshotSizing.nextWidth(
            requestedWidth = 540,
            actualWidth = 540,
            encodedBytes = 1024,
            maxEncodedBytes = 2 * 1024 * 1024,
        )

        assertNull(next)
    }
}
