package ai.zeroclaw.android.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.unit.dp
import androidx.lifecycle.repeatOnLifecycle
import ai.zeroclaw.android.data.ZeroClawSettings
import ai.zeroclaw.android.util.BatteryUtils

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsScreen(
    settings: ZeroClawSettings,
    onSettingsChange: (ZeroClawSettings) -> Unit,
    onSave: () -> Unit,
    onBack: () -> Unit
) {
    var showApiKey by remember { mutableStateOf(false) }
    var localSettings by remember(settings) { mutableStateOf(settings) }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Settings") },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.Default.ArrowBack, contentDescription = "Back")
                    }
                },
                actions = {
                    TextButton(onClick = {
                        onSettingsChange(localSettings)
                        onSave()
                    }) {
                        Text("Save")
                    }
                }
            )
        }
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .verticalScroll(rememberScrollState())
                .padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(24.dp)
        ) {
            // Provider Section
            SettingsSection(title = "AI Provider") {
                // Provider dropdown
                var providerExpanded by remember { mutableStateOf(false) }
                ExposedDropdownMenuBox(
                    expanded = providerExpanded,
                    onExpandedChange = { providerExpanded = it }
                ) {
                    OutlinedTextField(
                        value = localSettings.provider.replaceFirstChar { it.uppercase() },
                        onValueChange = {},
                        readOnly = true,
                        label = { Text("Provider") },
                        trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = providerExpanded) },
                        modifier = Modifier
                            .fillMaxWidth()
                            .menuAnchor()
                    )
                    ExposedDropdownMenu(
                        expanded = providerExpanded,
                        onDismissRequest = { providerExpanded = false }
                    ) {
                        listOf("anthropic", "openai", "google", "openrouter").forEach { provider ->
                            DropdownMenuItem(
                                text = { Text(provider.replaceFirstChar { it.uppercase() }) },
                                onClick = {
                                    localSettings = localSettings.copy(provider = provider)
                                    providerExpanded = false
                                }
                            )
                        }
                    }
                }

                Spacer(modifier = Modifier.height(12.dp))

                // Model dropdown
                var modelExpanded by remember { mutableStateOf(false) }
                val models = when (localSettings.provider) {
                    "anthropic" -> listOf(
                        "claude-opus-4-5" to "Claude Opus 4.5",
                        "claude-sonnet-4-5" to "Claude Sonnet 4.5",
                        "claude-haiku-3-5" to "Claude Haiku 3.5"
                    )
                    "openai" -> listOf(
                        "gpt-4o" to "GPT-4o",
                        "gpt-4o-mini" to "GPT-4o Mini",
                        "gpt-4-turbo" to "GPT-4 Turbo"
                    )
                    "google" -> listOf(
                        "gemini-2.5-pro" to "Gemini 2.5 Pro",
                        "gemini-2.5-flash" to "Gemini 2.5 Flash"
                    )
                    else -> listOf("auto" to "Auto")
                }

                ExposedDropdownMenuBox(
                    expanded = modelExpanded,
                    onExpandedChange = { modelExpanded = it }
                ) {
                    OutlinedTextField(
                        value = models.find { it.first == localSettings.model }?.second ?: localSettings.model,
                        onValueChange = {},
                        readOnly = true,
                        label = { Text("Model") },
                        trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = modelExpanded) },
                        modifier = Modifier
                            .fillMaxWidth()
                            .menuAnchor()
                    )
                    ExposedDropdownMenu(
                        expanded = modelExpanded,
                        onDismissRequest = { modelExpanded = false }
                    ) {
                        models.forEach { (id, name) ->
                            DropdownMenuItem(
                                text = { Text(name) },
                                onClick = {
                                    localSettings = localSettings.copy(model = id)
                                    modelExpanded = false
                                }
                            )
                        }
                    }
                }

                Spacer(modifier = Modifier.height(12.dp))

                // API Key
                OutlinedTextField(
                    value = localSettings.apiKey,
                    onValueChange = { localSettings = localSettings.copy(apiKey = it) },
                    label = { Text("API Key") },
                    placeholder = { Text("sk-ant-...") },
                    visualTransformation = if (showApiKey) VisualTransformation.None else PasswordVisualTransformation(),
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
                    trailingIcon = {
                        IconButton(onClick = { showApiKey = !showApiKey }) {
                            Icon(
                                if (showApiKey) Icons.Default.VisibilityOff else Icons.Default.Visibility,
                                contentDescription = if (showApiKey) "Hide" else "Show"
                            )
                        }
                    },
                    modifier = Modifier.fillMaxWidth(),
                    singleLine = true
                )

                Text(
                    text = "Your API key is stored securely in Android Keystore",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(top = 4.dp)
                )
            }

            // Behavior Section
            SettingsSection(title = "Behavior") {
                SettingsSwitch(
                    title = "Auto-start on boot",
                    description = "Start ZeroClaw when device boots",
                    checked = localSettings.autoStart,
                    onCheckedChange = { localSettings = localSettings.copy(autoStart = it) }
                )

                SettingsSwitch(
                    title = "Notifications",
                    description = "Show agent messages as notifications",
                    checked = localSettings.notificationsEnabled,
                    onCheckedChange = { localSettings = localSettings.copy(notificationsEnabled = it) }
                )
            }

            // System Prompt Section
            SettingsSection(title = "System Prompt") {
                OutlinedTextField(
                    value = localSettings.systemPrompt,
                    onValueChange = { localSettings = localSettings.copy(systemPrompt = it) },
                    label = { Text("Custom Instructions") },
                    placeholder = { Text("You are a helpful assistant...") },
                    modifier = Modifier
                        .fillMaxWidth()
                        .height(120.dp),
                    maxLines = 5
                )
            }

            // Battery Optimization Section
            val context = LocalContext.current
            val lifecycleOwner = androidx.lifecycle.compose.LocalLifecycleOwner.current
            var isOptimized by remember { mutableStateOf(BatteryUtils.isIgnoringBatteryOptimizations(context)) }

            // Refresh battery optimization state when screen resumes
            LaunchedEffect(lifecycleOwner) {
                lifecycleOwner.lifecycle.repeatOnLifecycle(androidx.lifecycle.Lifecycle.State.RESUMED) {
                    isOptimized = BatteryUtils.isIgnoringBatteryOptimizations(context)
                }
            }

            SettingsSection(title = "Battery") {
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceBetween,
                    verticalAlignment = Alignment.CenterVertically
                ) {
                    Column(modifier = Modifier.weight(1f)) {
                        Text("Battery Optimization")
                        Text(
                            text = if (isOptimized) "Unrestricted ✓" else "Restricted - may affect background tasks",
                            style = MaterialTheme.typography.bodySmall,
                            color = if (isOptimized) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.error
                        )
                    }
                    if (!isOptimized) {
                        TextButton(onClick = {
                            BatteryUtils.requestBatteryOptimizationExemption(context)
                        }) {
                            Text("Fix")
                        }
                    }
                }

                if (BatteryUtils.hasAggressiveBatteryOptimization()) {
                    Spacer(modifier = Modifier.height(8.dp))
                    Text(
                        text = "⚠️ Your device may have aggressive battery management. If ZeroClaw stops working in background, check manufacturer battery settings.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant
                    )
                }
            }

            // About Section
            SettingsSection(title = "About") {
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceBetween
                ) {
                    Text("Version")
                    Text("0.1.0", color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
                Spacer(modifier = Modifier.height(8.dp))
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceBetween
                ) {
                    Text("ZeroClaw Core")
                    Text("0.x.x", color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
        }
    }
}

@Composable
fun SettingsSection(
    title: String,
    content: @Composable ColumnScope.() -> Unit
) {
    Column {
        Text(
            text = title,
            style = MaterialTheme.typography.titleSmall,
            color = MaterialTheme.colorScheme.primary,
            modifier = Modifier.padding(bottom = 12.dp)
        )
        Surface(
            color = MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.5f),
            shape = MaterialTheme.shapes.medium
        ) {
            Column(
                modifier = Modifier.padding(16.dp),
                content = content
            )
        }
    }
}

@Composable
fun SettingsSwitch(
    title: String,
    description: String,
    checked: Boolean,
    onCheckedChange: (Boolean) -> Unit
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(text = title)
            Text(
                text = description,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant
            )
        }
        Switch(
            checked = checked,
            onCheckedChange = onCheckedChange
        )
    }
}
