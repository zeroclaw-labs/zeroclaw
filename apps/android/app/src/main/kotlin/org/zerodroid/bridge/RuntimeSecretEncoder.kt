package org.zerodroid.bridge

import java.io.File
import java.io.FileOutputStream
import java.security.SecureRandom
import javax.crypto.Cipher
import javax.crypto.spec.IvParameterSpec
import javax.crypto.spec.SecretKeySpec

/** Writes ZeroClaw-compatible `enc2:` values so config.toml never materializes raw credentials. */
internal class RuntimeSecretEncoder(private val configDir: File) {
    fun encrypt(plaintext: String): String {
        if (plaintext.isEmpty()) return plaintext
        val key = loadOrCreateKey()
        return try {
            val nonce = ByteArray(NONCE_BYTES).also(SecureRandom()::nextBytes)
            val cipher = Cipher.getInstance("ChaCha20-Poly1305")
            cipher.init(
                Cipher.ENCRYPT_MODE,
                SecretKeySpec(key, "ChaCha20"),
                IvParameterSpec(nonce),
            )
            val ciphertextAndTag = cipher.doFinal(plaintext.toByteArray(Charsets.UTF_8))
            "enc2:" + (nonce + ciphertextAndTag).toHex()
        } finally {
            key.fill(0)
        }
    }

    private fun loadOrCreateKey(): ByteArray = synchronized(KEY_LOCK) {
        configDir.mkdirs()
        val keyFile = File(configDir, ".secret_key")
        readKey(keyFile)?.let { return@synchronized it }
        if (keyFile.exists()) {
            throw IllegalStateException("Invalid ZeroClaw master key at ${keyFile.path}")
        }
        val key = ByteArray(KEY_BYTES).also(SecureRandom()::nextBytes)
        val tmp = File(configDir, ".secret_key.tmp-${System.nanoTime()}")
        FileOutputStream(tmp).use { output ->
            output.write(key.toHex().toByteArray(Charsets.US_ASCII))
            output.fd.sync()
        }
        runCatching { android.system.Os.chmod(tmp.absolutePath, 384 /* 0600 */) }
        if (!tmp.renameTo(keyFile)) {
            tmp.delete()
            readKey(keyFile)?.let { return@synchronized it }
            throw IllegalStateException("Could not publish ZeroClaw master key")
        }
        runCatching { android.system.Os.chmod(keyFile.absolutePath, 384 /* 0600 */) }
        key
    }

    private fun readKey(file: File): ByteArray? {
        if (!file.isFile) return null
        val encoded = file.readText().trim()
        if (encoded.length != KEY_BYTES * 2 || encoded.any { it.digitToIntOrNull(16) == null }) {
            return null
        }
        return ByteArray(KEY_BYTES) { index ->
            encoded.substring(index * 2, index * 2 + 2).toInt(16).toByte()
        }
    }

    private fun ByteArray.toHex(): String = joinToString("") { "%02x".format(it) }

    companion object {
        private const val KEY_BYTES = 32
        private const val NONCE_BYTES = 12
        private val KEY_LOCK = Any()
    }
}
