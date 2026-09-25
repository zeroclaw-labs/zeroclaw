package org.zerodroid.bridge

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.nio.file.Files
import javax.crypto.Cipher
import javax.crypto.spec.IvParameterSpec
import javax.crypto.spec.SecretKeySpec

class RuntimeSecretEncoderTest {
    @Test
    fun emitsRustCompatibleEnc2CiphertextAndPersistentKey() {
        val dir = Files.createTempDirectory("zerodroid-secret").toFile()
        val encoded = RuntimeSecretEncoder(dir).encrypt("provider-secret")
        assertTrue(encoded.startsWith("enc2:"))
        assertFalse(encoded.contains("provider-secret"))

        val key = dir.resolve(".secret_key").readText().trim().chunked(2)
            .map { it.toInt(16).toByte() }.toByteArray()
        val blob = encoded.removePrefix("enc2:").chunked(2)
            .map { it.toInt(16).toByte() }.toByteArray()
        val cipher = Cipher.getInstance("ChaCha20-Poly1305")
        cipher.init(
            Cipher.DECRYPT_MODE,
            SecretKeySpec(key, "ChaCha20"),
            IvParameterSpec(blob.copyOfRange(0, 12)),
        )
        val plaintext = cipher.doFinal(blob.copyOfRange(12, blob.size)).toString(Charsets.UTF_8)
        assertEquals("provider-secret", plaintext)
        dir.deleteRecursively()
    }

    @Test
    fun samePlaintextUsesFreshNonce() {
        val dir = Files.createTempDirectory("zerodroid-secret-nonce").toFile()
        val encoder = RuntimeSecretEncoder(dir)
        assertFalse(encoder.encrypt("same") == encoder.encrypt("same"))
        dir.deleteRecursively()
    }

    @Test
    fun plaintextBeginningWithEnc2IsStillEncrypted() {
        val dir = Files.createTempDirectory("zerodroid-secret-prefix").toFile()
        val encoded = RuntimeSecretEncoder(dir).encrypt("enc2:literal-provider-key")

        assertTrue(encoded.startsWith("enc2:"))
        assertFalse(encoded == "enc2:literal-provider-key")
        dir.deleteRecursively()
    }
}
