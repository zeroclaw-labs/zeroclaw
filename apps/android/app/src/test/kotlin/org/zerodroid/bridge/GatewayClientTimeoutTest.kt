package org.zerodroid.bridge

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File
import java.net.HttpURLConnection
import java.net.URL

class GatewayClientTimeoutTest {
    @Test
    fun clientRetainsRequestPastLegacy120SecondBoundary() {
        val connection = FakeHttpURLConnection()

        GatewayClient.configureConnectionTimeouts(
            connection,
            GeneratedAgentTurnContract.CLIENT_READ_TIMEOUT_MS,
        )

        val serverBudgetMs = GeneratedAgentTurnContract.TURN_TIMEOUT_SECS * 1_000
        val expectedMarginMs = GeneratedAgentTurnContract.TRANSPORT_MARGIN_SECS * 1_000
        assertTrue(connection.readTimeout > 120_000)
        assertTrue(connection.readTimeout > serverBudgetMs)
        assertEquals(expectedMarginMs, connection.readTimeout - serverBudgetMs)
        assertEquals(630_000, connection.readTimeout)
    }

    @Test
    fun generatedRuntimeAndGatewayShareTheTurnBudget() {
        assertEquals(
            "long_running_request_timeout_secs = 600",
            GeneratedAgentTurnContract.gatewayTimeoutConfigLine(),
        )
        assertEquals(
            "agentic_timeout_secs = 600",
            GeneratedAgentTurnContract.agenticTimeoutConfigLine(),
        )
    }

    @Test
    fun productionPathsUseTheSharedTurnContract() {
        val client = mainSource("kotlin/org/zerodroid/bridge/GatewayClient.kt")
        val config = mainSource("kotlin/org/zerodroid/bridge/ConfigStore.kt")

        assertEquals(
            1,
            client.countOccurrences(
                "readTimeoutMs = GeneratedAgentTurnContract.CLIENT_READ_TIMEOUT_MS",
            ),
        )
        assertEquals(1, client.countOccurrences("configureConnectionTimeouts(conn, readTimeoutMs)"))
        assertEquals(
            1,
            config.countOccurrences("GeneratedAgentTurnContract.gatewayTimeoutConfigLine()"),
        )
        assertEquals(
            1,
            config.countOccurrences("GeneratedAgentTurnContract.agenticTimeoutConfigLine()"),
        )
    }

    private class FakeHttpURLConnection :
        HttpURLConnection(URL("http://127.0.0.1/timeout-test")) {
        override fun connect() = Unit
        override fun disconnect() = Unit
        override fun usingProxy(): Boolean = false
    }

    private fun String.countOccurrences(needle: String): Int =
        windowed(needle.length).count { it == needle }

    private fun mainSource(relativePath: String): String {
        val roots = listOf(
            File("src/main"),
            File("app/src/main"),
            File("apps/android/app/src/main"),
        )
        val file = roots.asSequence()
            .map { it.resolve(relativePath) }
            .firstOrNull(File::isFile)
            ?: throw AssertionError("cannot locate Android main source file: $relativePath")
        return file.readText()
    }
}
