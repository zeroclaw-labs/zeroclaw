package org.zerodroid.bridge

import org.apache.sshd.common.util.net.SshdSocketAddress
import org.apache.sshd.server.forward.TcpForwardingFilter
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class SshTunnelPolicyTest {
    @Test
    fun allowsOnlyDirectForwardToLoopbackGateway() {
        assertTrue(
            SshTunnelPolicy.allows(
                TcpForwardingFilter.Type.Direct,
                SshdSocketAddress("127.0.0.1", 42617),
                42617,
            )
        )
        assertTrue(
            SshTunnelPolicy.allows(
                TcpForwardingFilter.Type.Direct,
                SshdSocketAddress("localhost", 42617),
                42617,
            )
        )
    }

    @Test
    fun rejectsOtherPortsHostsAndRemoteListeners() {
        assertFalse(
            SshTunnelPolicy.allows(
                TcpForwardingFilter.Type.Direct,
                SshdSocketAddress("127.0.0.1", 8470),
                42617,
            )
        )
        assertFalse(
            SshTunnelPolicy.allows(
                TcpForwardingFilter.Type.Direct,
                SshdSocketAddress("192.0.2.10", 42617),
                42617,
            )
        )
        assertFalse(
            SshTunnelPolicy.allows(
                TcpForwardingFilter.Type.Forwarded,
                SshdSocketAddress("127.0.0.1", 42617),
                42617,
            )
        )
    }
}
