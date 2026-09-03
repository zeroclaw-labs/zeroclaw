package org.zerodroid.bridge

/**
 * Lifecycle contract for turns started by the generated Android configuration.
 *
 * The runtime and synchronous gateway route share one server-owned budget. The overlay client
 * waits one transport margin longer so it receives the gateway's final response (including a
 * server timeout) before presenting a retryable error to the user.
 */
internal object GeneratedAgentTurnContract {
    const val TURN_TIMEOUT_SECS = 600
    const val TRANSPORT_MARGIN_SECS = 30
    const val CLIENT_READ_TIMEOUT_MS =
        (TURN_TIMEOUT_SECS + TRANSPORT_MARGIN_SECS) * 1_000

    fun gatewayTimeoutConfigLine(): String =
        "long_running_request_timeout_secs = $TURN_TIMEOUT_SECS"

    fun agenticTimeoutConfigLine(): String =
        "agentic_timeout_secs = $TURN_TIMEOUT_SECS"
}
