package org.zerodroid.bridge

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class GeneratedConfigPolicyTest {
    @Test
    fun zeroRouterLeadsThePickerWithGeminiFlashDefault() {
        val ordered = ProviderCatalog.orderForPicker(
            listOf(
                ProviderInfo("openai", "OpenAI"),
                ProviderInfo("zerorouter", "ZeroRouter"),
                ProviderInfo("anthropic", "Anthropic"),
            )
        )

        assertEquals("zerorouter", ordered.first().id)
        assertEquals("google/gemini-3.7-flash", ProviderCatalog.defaultModel("zerorouter"))
    }

    @Test
    fun retiredDeepSeekPrefillSelfHealsToCurrentFlashModel() {
        assertEquals("deepseek-v4-flash", ProviderCatalog.defaultModel("deepseek"))
        assertTrue("deepseek-chat" in ProviderCatalog.RETIRED_MODELS)
    }

    @Test
    fun generatedGatewayUsesPrivateNamespaceAndSecondFactor() {
        val section = GeneratedConfigPolicy.gatewayIsolation(
            pathPrefix = "/zd-unit-test-path",
            webhookSecret = "unit-test-webhook-secret",
            loopbackAdminSecret = "unit-test-admin-secret",
        ).joinToString("\n")

        assertTrue(section.contains("path_prefix = \"/zd-unit-test-path\""))
        assertTrue(section.contains("loopback_admin_secret = \"unit-test-admin-secret\""))
        assertTrue(section.contains("[channels.webhook.zerodroid_internal]"))
        assertTrue(section.contains("enabled = false"))
        assertTrue(section.contains("secret = \"unit-test-webhook-secret\""))
    }

    @Test
    fun configPreviewRedactsProviderAndWebhookSecrets() {
        val preview = GeneratedConfigPolicy.redactSecrets(
            "api_key = \"provider-value\"\nsecret = \"gateway-value\"\n" +
                "loopback_admin_secret = \"admin-value\"\npath_prefix = \"/zd-visible\"\n"
        )

        assertFalse(preview.contains("provider-value"))
        assertFalse(preview.contains("gateway-value"))
        assertFalse(preview.contains("admin-value"))
        assertTrue(preview.contains("path_prefix = \"/zd-visible\""))
    }

    @Test
    fun freshInstallKeepsAndroidToolsDisabledAndActionsGuarded() {
        val section = GeneratedConfigPolicy.androidSection(
            enabled = false,
            autonomousControl = false,
            socketPath = "/data/user/10/org.zerodroid.bridge/files/ui.sock",
        ).joinToString("\n")

        assertTrue(section.contains("enabled = false"))
        assertTrue(section.contains("socket_path = \"/data/user/10/org.zerodroid.bridge/files/ui.sock\""))
        assertTrue(section.contains("require_approval_for_actions = true"))
    }

    @Test
    fun readOnlyProfileExcludesEveryMutatingPhoneSurface() {
        val profile = GeneratedConfigPolicy.riskProfile(autonomous = false).joinToString("\n")

        assertTrue(profile.contains("level = \"readonly\""))
        assertTrue(profile.contains("workspace_only = true"))
        assertTrue(profile.contains("allowed_roots = []"))
        assertTrue(profile.contains("block_high_risk_commands = true"))
        assertTrue(profile.contains("android_action"))
        assertTrue(profile.contains("android_launch"))
        assertTrue(profile.contains("shell"))
        assertTrue(profile.contains("file_write"))
        assertFalse(profile.contains("level = \"full\""))
    }

    @Test
    fun autonomousProfileScopesPreApprovalToPhoneControl() {
        val android = GeneratedConfigPolicy.androidSection(
            enabled = true,
            autonomousControl = true,
            socketPath = "/data/user/0/org.zerodroid.bridge/files/ui.sock",
        ).joinToString("\n")
        val profile = GeneratedConfigPolicy.riskProfile(autonomous = true).joinToString("\n")

        assertTrue(android.contains("require_approval_for_actions = false"))
        assertTrue(profile.contains("level = \"supervised\""))
        assertTrue(profile.contains("workspace_only = true"))
        assertTrue(profile.contains("android_action"))
        assertTrue(profile.contains("android_launch"))
        assertTrue(profile.contains("\"am\""))
        assertTrue(profile.contains("\"dumpsys\""))
        assertTrue(profile.contains("always_ask = [\"shell\", \"bash\", \"file_write\"]"))
        assertTrue(profile.contains("excluded_tools = []"))
        assertFalse(profile.contains("level = \"full\""))
    }

    @Test
    fun autonomousFlagCannotBypassDisabledAndroidFamily() {
        val section = GeneratedConfigPolicy.androidSection(
            enabled = false,
            autonomousControl = true,
            socketPath = "/data/user/0/org.zerodroid.bridge/files/ui.sock",
        ).joinToString("\n")

        assertTrue(section.contains("enabled = false"))
        assertTrue(section.contains("require_approval_for_actions = true"))
    }
}
