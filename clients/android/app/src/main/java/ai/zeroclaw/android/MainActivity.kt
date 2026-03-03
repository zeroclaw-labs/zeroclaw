package ai.zeroclaw.android

import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import ai.zeroclaw.android.bridge.ZeroClawBridge
import ai.zeroclaw.android.bridge.AgentStatus
import ai.zeroclaw.android.data.ZeroClawSettings
import ai.zeroclaw.android.service.ZeroClawService
import ai.zeroclaw.android.ui.SettingsScreen
import ai.zeroclaw.android.ui.theme.ZeroClawTheme
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            ZeroClawTheme {
                Surface(
                    modifier = Modifier.fillMaxSize(),
                    color = MaterialTheme.colorScheme.background
                ) {
                    ZeroClawMainApp(
                        onStartService = { startAgentService() },
                        onStopService = { stopAgentService() }
                    )
                }
            }
        }
    }

    private fun startAgentService() {
        val intent = Intent(this, ZeroClawService::class.java).apply {
            action = ZeroClawService.ACTION_START
        }
        startForegroundService(intent)
    }

    private fun stopAgentService() {
        val intent = Intent(this, ZeroClawService::class.java).apply {
            action = ZeroClawService.ACTION_STOP
        }
        startService(intent)
    }
}

// ── Navigation ──────────────────────────────────────────────────

enum class Screen { Chat, Settings }

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ZeroClawMainApp(
    onStartService: () -> Unit,
    onStopService: () -> Unit
) {
    var currentScreen by remember { mutableStateOf(Screen.Chat) }
    val app = ZeroClawApp.instance
    val scope = rememberCoroutineScope()
    val settings by app.settingsRepository.settings.collectAsState(
        initial = ZeroClawSettings()
    )
    val isFirstRun by app.settingsRepository.isFirstRun.collectAsState(initial = true)

    // Show setup wizard on first run
    if (isFirstRun) {
        SetupWizardScreen(
            onComplete = { provider, apiKey ->
                scope.launch {
                    app.settingsRepository.updateSettings(
                        settings.copy(provider = provider, apiKey = apiKey)
                    )
                    app.settingsRepository.setFirstRunComplete()
                    // Auto-start agent after setup
                    onStartService()
                }
            }
        )
        return
    }

    when (currentScreen) {
        Screen.Chat -> ChatScreen(
            settings = settings,
            onOpenSettings = { currentScreen = Screen.Settings },
            onStartService = onStartService,
            onStopService = onStopService
        )
        Screen.Settings -> SettingsScreen(
            settings = settings,
            onSettingsChange = { newSettings ->
                scope.launch { app.settingsRepository.updateSettings(newSettings) }
            },
            onSave = {
                // Apply config to native bridge if loaded
                if (ZeroClawBridge.isLoaded()) {
                    ZeroClawBridge.updateApiKey(settings.provider, settings.apiKey)
                }
                currentScreen = Screen.Chat
            },
            onBack = { currentScreen = Screen.Chat }
        )
    }
}

// ── Setup Wizard ────────────────────────────────────────────────

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SetupWizardScreen(onComplete: (provider: String, apiKey: String) -> Unit) {
    var step by remember { mutableIntStateOf(0) }
    var selectedProvider by remember { mutableStateOf("openrouter") }
    var apiKey by remember { mutableStateOf("") }

    val providers = listOf(
        "openrouter" to "OpenRouter (200+ models)",
        "anthropic" to "Anthropic (Claude)",
        "openai" to "OpenAI (GPT-4o)",
        "google" to "Google (Gemini)",
        "ollama" to "Ollama (Local, free)"
    )

    Scaffold(
        topBar = {
            TopAppBar(title = { Text("Welcome to MoA") })
        }
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .padding(24.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.Center
        ) {
            when (step) {
                // Step 0: Welcome
                0 -> {
                    Text(
                        text = "MoA",
                        style = MaterialTheme.typography.displayLarge,
                        color = MaterialTheme.colorScheme.primary
                    )
                    Spacer(modifier = Modifier.height(8.dp))
                    Text(
                        text = "Master of AI",
                        style = MaterialTheme.typography.headlineSmall
                    )
                    Spacer(modifier = Modifier.height(16.dp))
                    Text(
                        text = "Your AI assistant, running locally on your device.\nAll messages stay private.",
                        style = MaterialTheme.typography.bodyLarge,
                        textAlign = TextAlign.Center,
                        color = MaterialTheme.colorScheme.onSurfaceVariant
                    )
                    Spacer(modifier = Modifier.height(48.dp))
                    Button(
                        onClick = { step = 1 },
                        modifier = Modifier.fillMaxWidth()
                    ) {
                        Text("Get Started")
                    }
                }

                // Step 1: Choose provider
                1 -> {
                    Text(
                        text = "Choose your AI provider",
                        style = MaterialTheme.typography.headlineSmall,
                        modifier = Modifier.padding(bottom = 8.dp)
                    )
                    Text(
                        text = "You can change this later in Settings",
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(bottom = 24.dp)
                    )

                    providers.forEach { (id, label) ->
                        OutlinedCard(
                            onClick = { selectedProvider = id },
                            modifier = Modifier
                                .fillMaxWidth()
                                .padding(vertical = 4.dp),
                            border = CardDefaults.outlinedCardBorder().let { border ->
                                if (selectedProvider == id)
                                    androidx.compose.foundation.BorderStroke(2.dp, MaterialTheme.colorScheme.primary)
                                else border
                            }
                        ) {
                            Row(
                                modifier = Modifier.padding(16.dp),
                                verticalAlignment = Alignment.CenterVertically
                            ) {
                                RadioButton(
                                    selected = selectedProvider == id,
                                    onClick = { selectedProvider = id }
                                )
                                Spacer(modifier = Modifier.width(12.dp))
                                Text(label)
                            }
                        }
                    }

                    Spacer(modifier = Modifier.height(24.dp))

                    Button(
                        onClick = {
                            if (selectedProvider == "ollama") {
                                onComplete(selectedProvider, "")
                            } else {
                                step = 2
                            }
                        },
                        modifier = Modifier.fillMaxWidth()
                    ) {
                        Text("Next")
                    }
                }

                // Step 2: API Key
                2 -> {
                    Text(
                        text = "Enter your API key",
                        style = MaterialTheme.typography.headlineSmall,
                        modifier = Modifier.padding(bottom = 8.dp)
                    )
                    Text(
                        text = "Your key is stored securely on this device only.",
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(bottom = 24.dp)
                    )

                    OutlinedTextField(
                        value = apiKey,
                        onValueChange = { apiKey = it },
                        label = { Text("API Key") },
                        placeholder = { Text("sk-... or AIza...") },
                        modifier = Modifier.fillMaxWidth(),
                        singleLine = true
                    )

                    Spacer(modifier = Modifier.height(24.dp))

                    Button(
                        onClick = { onComplete(selectedProvider, apiKey.trim()) },
                        modifier = Modifier.fillMaxWidth()
                    ) {
                        Text("Start Chatting")
                    }

                    Spacer(modifier = Modifier.height(12.dp))

                    TextButton(
                        onClick = { onComplete(selectedProvider, "") }
                    ) {
                        Text("Skip (use credits)")
                    }

                    Spacer(modifier = Modifier.height(12.dp))

                    TextButton(onClick = { step = 1 }) {
                        Text("Back")
                    }
                }
            }
        }
    }
}

// ── Chat Screen ─────────────────────────────────────────────────

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ChatScreen(
    settings: ZeroClawSettings,
    onOpenSettings: () -> Unit,
    onStartService: () -> Unit,
    onStopService: () -> Unit
) {
    var agentStatus by remember { mutableStateOf(AgentStatus.Stopped) }
    var messages by remember { mutableStateOf(listOf<ChatMessage>()) }
    var inputText by remember { mutableStateOf("") }
    var isThinking by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    val listState = rememberLazyListState()

    // Scroll to bottom when messages change
    LaunchedEffect(messages.size) {
        if (messages.isNotEmpty()) {
            listState.animateScrollToItem(messages.size - 1)
        }
    }

    // Refresh agent status periodically
    LaunchedEffect(Unit) {
        while (true) {
            if (ZeroClawBridge.isLoaded()) {
                agentStatus = ZeroClawBridge.getStatus()
            }
            kotlinx.coroutines.delay(3000)
        }
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("MoA") },
                actions = {
                    StatusIndicator(status = agentStatus)
                    IconButton(onClick = onOpenSettings) {
                        Icon(Icons.Default.Settings, contentDescription = "Settings")
                    }
                }
            )
        },
        bottomBar = {
            ChatInput(
                text = inputText,
                enabled = !isThinking && (agentStatus == AgentStatus.Running || ZeroClawBridge.isLoaded()),
                onTextChange = { inputText = it },
                onSend = {
                    if (inputText.isNotBlank() && !isThinking) {
                        val userMessage = inputText.trim()
                        messages = messages + ChatMessage(
                            content = userMessage,
                            isUser = true
                        )
                        inputText = ""
                        isThinking = true

                        // Send to ZeroClaw native bridge (local processing)
                        scope.launch {
                            try {
                                val response = ZeroClawBridge.sendMessage(userMessage).getOrThrow()
                                messages = messages + ChatMessage(
                                    content = response,
                                    isUser = false
                                )
                            } catch (e: Exception) {
                                messages = messages + ChatMessage(
                                    content = "Error: ${e.message ?: "Failed to get response"}",
                                    isUser = false,
                                    isError = true
                                )
                            } finally {
                                isThinking = false
                            }
                        }
                    }
                }
            )
        }
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
        ) {
            if (messages.isEmpty()) {
                EmptyState(
                    status = agentStatus,
                    isConfigured = settings.isConfigured(),
                    onStart = {
                        onStartService()
                        agentStatus = AgentStatus.Starting
                    },
                    onOpenSettings = onOpenSettings
                )
            } else {
                LazyColumn(
                    state = listState,
                    modifier = Modifier
                        .weight(1f)
                        .padding(horizontal = 16.dp),
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                    contentPadding = PaddingValues(vertical = 8.dp)
                ) {
                    items(messages) { message ->
                        ChatBubble(message = message)
                    }

                    if (isThinking) {
                        item {
                            ThinkingIndicator()
                        }
                    }
                }
            }
        }
    }
}

// ── UI Components ───────────────────────────────────────────────

@Composable
fun StatusIndicator(status: AgentStatus) {
    val (color, text) = when (status) {
        AgentStatus.Running -> MaterialTheme.colorScheme.primary to "Running"
        AgentStatus.Starting -> MaterialTheme.colorScheme.tertiary to "Starting"
        AgentStatus.Thinking -> MaterialTheme.colorScheme.secondary to "Thinking"
        AgentStatus.Stopped -> MaterialTheme.colorScheme.outline to "Stopped"
        AgentStatus.Error -> MaterialTheme.colorScheme.error to "Error"
    }

    Surface(
        color = color.copy(alpha = 0.2f),
        shape = MaterialTheme.shapes.small
    ) {
        Text(
            text = text,
            modifier = Modifier.padding(horizontal = 12.dp, vertical = 4.dp),
            color = color,
            style = MaterialTheme.typography.labelMedium
        )
    }
}

@Composable
fun EmptyState(
    status: AgentStatus,
    isConfigured: Boolean,
    onStart: () -> Unit,
    onOpenSettings: () -> Unit
) {
    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(32.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center
    ) {
        Text(
            text = "MoA",
            style = MaterialTheme.typography.displayLarge,
            color = MaterialTheme.colorScheme.primary
        )
        Spacer(modifier = Modifier.height(8.dp))
        Text(
            text = "Master of AI",
            style = MaterialTheme.typography.headlineSmall
        )
        Spacer(modifier = Modifier.height(8.dp))
        Text(
            text = "Your AI assistant, running locally",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            textAlign = TextAlign.Center
        )
        Spacer(modifier = Modifier.height(32.dp))

        when {
            !isConfigured -> {
                Text(
                    text = "Set up your AI provider to get started",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    textAlign = TextAlign.Center,
                    modifier = Modifier.padding(bottom = 16.dp)
                )
                Button(onClick = onOpenSettings) {
                    Text("Open Settings")
                }
            }
            status == AgentStatus.Stopped -> {
                Button(onClick = onStart) {
                    Text("Start Agent")
                }
            }
            status == AgentStatus.Starting -> {
                CircularProgressIndicator()
                Spacer(modifier = Modifier.height(16.dp))
                Text(
                    text = "Starting AI engine...",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant
                )
            }
            status == AgentStatus.Running -> {
                Text(
                    text = "Send a message to start chatting",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant
                )
            }
            else -> {}
        }
    }
}

@Composable
fun ChatInput(
    text: String,
    enabled: Boolean,
    onTextChange: (String) -> Unit,
    onSend: () -> Unit
) {
    Surface(tonalElevation = 3.dp) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(8.dp),
            verticalAlignment = Alignment.CenterVertically
        ) {
            OutlinedTextField(
                value = text,
                onValueChange = onTextChange,
                modifier = Modifier.weight(1f),
                placeholder = {
                    Text(if (enabled) "Message MoA..." else "Starting...")
                },
                singleLine = true,
                enabled = enabled
            )
            Spacer(modifier = Modifier.width(8.dp))
            FilledIconButton(
                onClick = onSend,
                enabled = enabled && text.isNotBlank()
            ) {
                Icon(
                    painter = androidx.compose.ui.res.painterResource(android.R.drawable.ic_menu_send),
                    contentDescription = "Send"
                )
            }
        }
    }
}

@Composable
fun ThinkingIndicator() {
    Row(
        modifier = Modifier.padding(vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically
    ) {
        Surface(
            color = MaterialTheme.colorScheme.primaryContainer,
            shape = MaterialTheme.shapes.medium
        ) {
            Row(modifier = Modifier.padding(12.dp)) {
                CircularProgressIndicator(
                    modifier = Modifier.size(16.dp),
                    strokeWidth = 2.dp
                )
                Spacer(modifier = Modifier.width(8.dp))
                Text(
                    text = "Thinking...",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onPrimaryContainer
                )
            }
        }
    }
}

@Composable
fun ChatBubble(message: ChatMessage) {
    val alignment = if (message.isUser) Alignment.End else Alignment.Start
    val color = when {
        message.isError -> MaterialTheme.colorScheme.errorContainer
        message.isUser -> MaterialTheme.colorScheme.primaryContainer
        else -> MaterialTheme.colorScheme.surfaceVariant
    }
    val textColor = when {
        message.isError -> MaterialTheme.colorScheme.onErrorContainer
        message.isUser -> MaterialTheme.colorScheme.onPrimaryContainer
        else -> MaterialTheme.colorScheme.onSurfaceVariant
    }

    Box(
        modifier = Modifier.fillMaxWidth(),
        contentAlignment = if (message.isUser) Alignment.CenterEnd else Alignment.CenterStart
    ) {
        Surface(
            color = color,
            shape = MaterialTheme.shapes.medium,
            modifier = Modifier.widthIn(max = 320.dp)
        ) {
            Text(
                text = message.content,
                modifier = Modifier.padding(12.dp),
                color = textColor
            )
        }
    }
}

// ── Data ────────────────────────────────────────────────────────

data class ChatMessage(
    val content: String,
    val isUser: Boolean,
    val isError: Boolean = false,
    val timestamp: Long = System.currentTimeMillis()
)
