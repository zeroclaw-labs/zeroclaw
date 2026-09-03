package org.zerodroid.bridge

import android.content.pm.ServiceInfo
import org.junit.Assert.assertEquals
import org.junit.Test

class BridgeForegroundPolicyTest {
    @Test
    fun agentSupervisorUsesOnlySpecialUseOnModernAndroid() {
        val type = BridgeForegroundPolicy.serviceType(34)

        assertEquals(ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE, type)
        assertEquals(0, type and ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE)
    }

    @Test
    fun legacyStartForegroundOmitsTheTypedMask() {
        assertEquals(0, BridgeForegroundPolicy.serviceType(33))
    }
}
