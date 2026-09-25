package org.zerodroid.bridge

import kotlin.math.floor
import kotlin.math.sqrt

/** Pure sizing policy for retrying a PNG that exceeds the bridge's encoded payload cap. */
internal object ScreenshotSizing {
    const val MIN_WIDTH = 64

    /**
     * Return a strictly smaller width for the next capture, or null when no smaller supported
     * capture exists. PNG byte size scales roughly with pixel area, so use the square root of the
     * required byte ratio and leave ten percent headroom for content-dependent compression.
     */
    fun nextWidth(
        requestedWidth: Int,
        actualWidth: Int,
        encodedBytes: Int,
        maxEncodedBytes: Int,
    ): Int? {
        if (encodedBytes <= maxEncodedBytes) return null
        val current = minOf(requestedWidth, actualWidth).coerceAtLeast(MIN_WIDTH)
        if (current <= MIN_WIDTH) return null

        val ratio = sqrt(maxEncodedBytes.toDouble() / encodedBytes.toDouble()) * 0.9
        val estimated = floor(current * ratio).toInt()
        return estimated.coerceIn(MIN_WIDTH, current - 1)
    }
}
