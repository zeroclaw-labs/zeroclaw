package ai.zeroclaw.android.util

import android.annotation.SuppressLint
import android.content.Context
import android.os.Build
import android.provider.Settings
import java.security.MessageDigest

/**
 * Generates a deterministic device fingerprint based on hardware-level attributes.
 *
 * Uses [Build.FINGERPRINT] (firmware code) as the primary identifier, combined with
 * [Settings.Secure.ANDROID_ID] for per-device uniqueness. This survives app reinstalls
 * and enables the server to deduplicate the same physical device.
 *
 * The fingerprint is a SHA-256 hash — no raw hardware identifiers are transmitted.
 *
 * Components:
 * - Build.FINGERPRINT: firmware build string (e.g., "samsung/a52q/a52q:14/UP1A.231005.007/...")
 * - Build.BOARD: hardware board name
 * - Build.HARDWARE: hardware name
 * - ANDROID_ID: 64-bit hex per-device (reset on factory reset only)
 */
object DeviceFingerprint {

    /**
     * Generate a deterministic fingerprint for this physical device.
     * Returns a 64-character hex SHA-256 hash.
     */
    @SuppressLint("HardwareIds")
    fun generate(context: Context): String {
        val androidId = Settings.Secure.getString(
            context.contentResolver,
            Settings.Secure.ANDROID_ID
        ) ?: ""

        // Build.FINGERPRINT is the "firmware code" — unique per build image
        val firmwareCode = Build.FINGERPRINT
        val board = Build.BOARD
        val hardware = Build.HARDWARE

        val raw = "zeroclaw-fp:$firmwareCode:$board:$hardware:$androidId"
        return sha256(raw)
    }

    /**
     * Get the raw firmware code (Build.FINGERPRINT) for logging/debug purposes.
     * This is NOT sent to the server — only the hashed fingerprint is.
     */
    fun firmwareCode(): String = Build.FINGERPRINT

    private fun sha256(input: String): String {
        val digest = MessageDigest.getInstance("SHA-256")
        val hash = digest.digest(input.toByteArray(Charsets.UTF_8))
        return hash.joinToString("") { "%02x".format(it) }
    }
}
