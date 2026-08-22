package org.zerodroid.bridge

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class BridgeTokenTest {
    @Test
    fun acceptsOnlyFullLengthLowercaseHexTokens() {
        assertTrue(isValidLocalSecret("0123456789abcdef0123456789abcdef"))
        assertFalse(isValidLocalSecret(""))
        assertFalse(isValidLocalSecret("0123456789abcdef"))
        assertFalse(isValidLocalSecret("01234567-89ab-cdef-0123-456789abcdef"))
        assertFalse(isValidLocalSecret("0123456789ABCDEF0123456789ABCDEF"))
        assertFalse(isValidLocalSecret("g123456789abcdef0123456789abcdef"))
    }
}
