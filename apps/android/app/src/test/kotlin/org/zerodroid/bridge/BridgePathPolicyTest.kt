package org.zerodroid.bridge

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.nio.file.Files

class BridgePathPolicyTest {
    @Test
    fun acceptsDescendantAndRejectsSiblingWithSharedStringPrefix() {
        val root = Files.createTempDirectory("bridge-path-policy").toFile()
        val base = root.resolve("images").apply { mkdirs() }
        val inside = base.resolve("capture.png").apply { writeText("png") }
        val sibling = root.resolve("images-escape").apply { mkdirs() }
            .resolve("capture.png").apply { writeText("png") }

        assertTrue(BridgePathPolicy.containsCanonicalFile(base, inside))
        assertFalse(BridgePathPolicy.containsCanonicalFile(base, sibling))
        root.deleteRecursively()
    }

    @Test
    fun canonicalizationRejectsParentTraversal() {
        val root = Files.createTempDirectory("bridge-path-traversal").toFile()
        val base = root.resolve("images").apply { mkdirs() }
        val outside = root.resolve("secret.png").apply { writeText("png") }
        val traversed = base.resolve("../secret.png")

        assertFalse(BridgePathPolicy.containsCanonicalFile(base, traversed))
        assertTrue(outside.exists())
        root.deleteRecursively()
    }
}
