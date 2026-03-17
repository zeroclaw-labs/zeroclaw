export type Locale = "en" | "ko";

const STORAGE_KEY_LOCALE = "zeroclaw_locale";

export function getStoredLocale(): Locale {
  const stored = localStorage.getItem(STORAGE_KEY_LOCALE);
  if (stored === "en" || stored === "ko") return stored;
  const browserLang = navigator.language.toLowerCase();
  if (browserLang.startsWith("ko")) return "ko";
  return "en";
}

export function setStoredLocale(locale: Locale): void {
  localStorage.setItem(STORAGE_KEY_LOCALE, locale);
}

type TranslationKey =
  | "app_title"
  | "new_chat"
  | "settings"
  | "chat"
  | "send"
  | "type_message"
  | "thinking"
  | "error_occurred"
  | "retry"
  | "advanced_settings"
  | "server_url"
  | "username"
  | "password"
  | "password_confirm"
  | "pairing_code"
  | "pairing_code_optional"
  | "pair"
  | "pairing"
  | "connect"
  | "connecting"
  | "disconnect"
  | "connection_status"
  | "connected"
  | "disconnected"
  | "token"
  | "health_check"
  | "checking"
  | "server_healthy"
  | "server_unreachable"
  | "pair_success"
  | "pair_failed"
  | "language"
  | "no_chats"
  | "welcome_title"
  | "welcome_subtitle"
  | "welcome_hint"
  | "delete_chat"
  | "delete_confirm"
  | "model"
  | "back_to_chat"
  | "not_connected_hint"
  | "sync_status"
  | "sync_connected"
  | "sync_disconnected"
  | "sync_device_id"
  | "sync_trigger"
  | "sync_triggering"
  | "sync_triggered"
  | "sync_failed"
  | "platform"
  // Auth flow
  | "login"
  | "login_title"
  | "login_subtitle"
  | "login_button"
  | "logging_in"
  | "login_failed"
  | "signup"
  | "signup_title"
  | "signup_subtitle"
  | "signup_button"
  | "signing_up"
  | "signup_success"
  | "signup_failed"
  | "no_account"
  | "have_account"
  | "logout"
  | "logout_confirm"
  // Interpreter
  | "interpreter"
  | "interpreter_idle"
  | "interpreter_connecting"
  | "interpreter_ready"
  | "interpreter_listening"
  | "interpreter_error"
  | "interpreter_stopping"
  | "interpreter_start"
  | "interpreter_stop"
  | "interpreter_bidirectional"
  | "interpreter_hint"
  | "interpreter_listening_hint"
  // Device selection
  | "select_device"
  | "select_device_subtitle"
  | "device_online"
  | "device_offline"
  | "device_this"
  | "device_remote"
  | "device_select"
  | "device_no_devices"
  | "device_pairing_required"
  | "enter_pairing_code"
  | "verify_pairing"
  | "verifying"
  | "pairing_verified"
  | "pairing_invalid"
  | "auto_connecting"
  // Device management in settings
  | "my_devices"
  | "device_name"
  | "device_platform"
  | "device_last_seen"
  | "set_pairing_code"
  | "change_pairing_code"
  | "remove_pairing_code"
  | "pairing_code_set"
  | "pairing_code_removed"
  | "new_pairing_code"
  | "save_pairing_code"
  | "account_info"
  // API Keys & Credits
  | "api_keys"
  | "api_key_claude"
  | "api_key_openai"
  | "api_key_gemini"
  | "api_key_placeholder"
  | "api_key_saved"
  | "api_key_cleared"
  | "api_key_hint"
  | "credits"
  | "credit_balance"
  | "credit_buy"
  | "credit_history"
  | "credit_package_basic"
  | "credit_package_standard"
  | "credit_package_premium"
  | "credit_package_pro"
  | "credit_operator_hint"
  // LLM / Model selection
  | "llm_settings"
  | "llm_provider"
  | "llm_model"
  | "llm_provider_claude"
  | "llm_provider_openai"
  | "llm_provider_gemini"
  | "connection_mode"
  | "connection_mode_relay"
  | "connection_mode_local"
  | "connection_mode_relay_hint"
  | "connection_mode_local_hint"
  | "advanced_settings_toggle"
  | "local_gateway_url"
  // Sidebar sections
  | "sidebar_devices"
  | "sidebar_channels"
  | "sidebar_tools"
  | "sidebar_no_devices"
  | "sidebar_no_channels"
  | "sidebar_no_tools"
  | "sidebar_chats"
  | "greeting_prompt"
  | "greeting_prompt_returning"
  // Lock screen
  | "lock_title"
  | "lock_subtitle"
  | "lock_password_placeholder"
  | "lock_unlock"
  | "lock_unlocking"
  | "lock_failed"
  | "lock_logout"
  // Workspace
  | "connect_folder"
  | "connect_github"
  | "connect_folder_hint"
  | "connect_github_hint"
  | "connect_github_placeholder"
  | "workspace_connected"
  | "repo_cloning";

const translations: Record<Locale, Record<TranslationKey, string>> = {
  en: {
    app_title: "MoA",
    new_chat: "New Chat",
    settings: "Settings",
    chat: "Chat",
    send: "Send",
    type_message: "Type a message...",
    thinking: "Thinking...",
    error_occurred: "An error occurred",
    retry: "Retry",
    advanced_settings: "Advanced Settings",
    server_url: "Server URL",
    username: "Email",
    password: "Password",
    password_confirm: "Confirm Password",
    pairing_code: "Pairing Code",
    pairing_code_optional: "Pairing Code (optional, for code-only mode)",
    pair: "Pair",
    pairing: "Pairing...",
    connect: "Connect",
    connecting: "Connecting...",
    disconnect: "Disconnect",
    connection_status: "Connection Status",
    connected: "Connected",
    disconnected: "Disconnected",
    token: "Token",
    health_check: "Health Check",
    checking: "Checking...",
    server_healthy: "Server is healthy",
    server_unreachable: "Server is unreachable",
    pair_success: "Successfully paired with server",
    pair_failed: "Pairing failed",
    language: "Language",
    no_chats: "No conversations yet",
    welcome_title: "Welcome to MoA",
    welcome_subtitle: "Your autonomous AI assistant",
    welcome_hint: "Start a conversation by typing a message below",
    delete_chat: "Delete",
    delete_confirm: "Delete this conversation?",
    model: "Model",
    back_to_chat: "Back to Chat",
    not_connected_hint: "Please login to start chatting",
    sync_status: "Sync Status",
    sync_connected: "Sync connected",
    sync_disconnected: "Sync not connected",
    sync_device_id: "Device ID",
    sync_trigger: "Full Sync",
    sync_triggering: "Syncing...",
    sync_triggered: "Full sync triggered successfully",
    sync_failed: "Sync failed",
    platform: "Platform",
    // Auth flow
    login: "Login",
    login_title: "MoA Login",
    login_subtitle: "Your autonomous AI assistant",
    login_button: "Login",
    logging_in: "Logging in...",
    login_failed: "Login failed. Please check your credentials.",
    signup: "Sign Up",
    signup_title: "Create Account",
    signup_subtitle: "MoA - Master of AI",
    signup_button: "Create Account",
    signing_up: "Creating account...",
    signup_success: "Account created! Please login.",
    signup_failed: "Registration failed",
    no_account: "Don't have an account?",
    have_account: "Already have an account?",
    logout: "Logout",
    logout_confirm: "Are you sure you want to logout?",
    // Interpreter
    interpreter: "Interpreter",
    interpreter_idle: "Ready",
    interpreter_connecting: "Connecting...",
    interpreter_ready: "Connected",
    interpreter_listening: "Listening",
    interpreter_error: "Error",
    interpreter_stopping: "Translating...",
    interpreter_start: "Start Interpretation",
    interpreter_stop: "Stop",
    interpreter_bidirectional: "Bidirectional",
    interpreter_hint: "Press Start to begin real-time voice interpretation",
    interpreter_listening_hint: "Listening... speak now",
    // Device selection
    select_device: "Select Device",
    select_device_subtitle: "Choose which MoA device to connect to",
    device_online: "Online",
    device_offline: "Offline",
    device_this: "This device",
    device_remote: "Remote",
    device_select: "Connect",
    device_no_devices: "No devices registered yet. This device will be registered automatically.",
    device_pairing_required: "Pairing code required for remote device",
    enter_pairing_code: "Enter pairing code",
    verify_pairing: "Verify",
    verifying: "Verifying...",
    pairing_verified: "Pairing verified!",
    pairing_invalid: "Invalid pairing code",
    auto_connecting: "Auto-connecting...",
    // Device management in settings
    my_devices: "My Devices",
    device_name: "Device Name",
    device_platform: "Platform",
    device_last_seen: "Last Seen",
    set_pairing_code: "Set Pairing Code",
    change_pairing_code: "Change Pairing Code",
    remove_pairing_code: "Remove Pairing Code",
    pairing_code_set: "Pairing code updated",
    pairing_code_removed: "Pairing code removed",
    new_pairing_code: "New pairing code",
    save_pairing_code: "Save",
    account_info: "Account",
    // API Keys & Credits
    api_keys: "API Keys",
    api_key_claude: "Claude (Anthropic)",
    api_key_openai: "OpenAI",
    api_key_gemini: "Gemini (Google)",
    api_key_placeholder: "sk-... or AIza...",
    api_key_saved: "API key saved",
    api_key_cleared: "API key removed",
    api_key_hint: "Use your own key for free. Without a key, operator credits are used (2x cost).",
    credits: "Credits",
    credit_balance: "Balance",
    credit_buy: "Buy Credits",
    credit_history: "History",
    credit_package_basic: "Basic",
    credit_package_standard: "Standard",
    credit_package_premium: "Premium",
    credit_package_pro: "Pro",
    credit_operator_hint: "Credits are used when you don't have your own API key (2x actual cost).",
    // LLM / Model selection
    llm_settings: "AI Model",
    llm_provider: "Provider",
    llm_model: "Model",
    llm_provider_claude: "Claude (Anthropic)",
    llm_provider_openai: "OpenAI",
    llm_provider_gemini: "Gemini (Google)",
    connection_mode: "Connection",
    connection_mode_relay: "Cloud (MoA Relay)",
    connection_mode_local: "Local Gateway",
    connection_mode_relay_hint: "Using MoA relay server. Credits are deducted per request.",
    connection_mode_local_hint: "Using your own API key via local gateway.",
    advanced_settings_toggle: "Advanced Settings",
    local_gateway_url: "Local Gateway URL",
    // Sidebar sections
    sidebar_devices: "Devices",
    sidebar_channels: "Channels",
    sidebar_tools: "Tools",
    sidebar_no_devices: "No devices",
    sidebar_no_channels: "No channels",
    sidebar_no_tools: "No tools",
    sidebar_chats: "Chats",
    greeting_prompt: "[SYSTEM] The user just logged in for the first time. Their username is \"{username}\". You are MoA, the user's personal AI assistant. Greet them with a polite, gentle, multi-step approach in a SINGLE response. Structure your response as follows:\n1. First, introduce yourself warmly: \"Hello. My name is MoA. To briefly introduce myself, I am your personal AI assistant. I look forward to working with you.\"\n2. Then, carefully and politely ask for their name and occupation: \"May I ask your name and what you do?\"\n3. Finally, ask how they would like to be addressed: \"If you don't mind, how would you prefer I address you?\"\nBe respectful, courteous, and gentle throughout — as if meeting someone important for the first time. Do NOT be overly casual. Use a warm but professional secretary-like tone. Respond in the user's language.",
    greeting_prompt_returning: "[SYSTEM] The user \"{username}\" just logged in. You are their personal AI assistant MoA. Greet them like a secretary who knows them. Use any memories you have about them (name, job, preferences). If you remember their real name, use it. Be warm, concise, and proactive — mention anything relevant or ask how you can help today.",
    // Lock screen
    lock_title: "MoA Locked",
    lock_subtitle: "Enter your password to unlock",
    lock_password_placeholder: "Password",
    lock_unlock: "Unlock",
    lock_unlocking: "Unlocking...",
    lock_failed: "Incorrect password. Please try again.",
    lock_logout: "Switch Account",
    connect_folder: "Connect Folder",
    connect_github: "Connect GitHub",
    connect_folder_hint: "Connect a local folder as workspace",
    connect_github_hint: "Clone a GitHub repo as workspace",
    connect_github_placeholder: "https://github.com/user/repo",
    workspace_connected: "Workspace connected",
    repo_cloning: "Cloning repository...",
  },
  ko: {
    app_title: "MoA",
    new_chat: "\uC0C8 \uB300\uD654",
    settings: "\uC124\uC815",
    chat: "\uCC44\uD305",
    send: "\uBCF4\uB0B4\uAE30",
    type_message: "\uBA54\uC2DC\uC9C0\uB97C \uC785\uB825\uD558\uC138\uC694...",
    thinking: "\uC0DD\uAC01 \uC911...",
    error_occurred: "\uC624\uB958\uAC00 \uBC1C\uC0DD\uD588\uC2B5\uB2C8\uB2E4",
    retry: "\uC7AC\uC2DC\uB3C4",
    advanced_settings: "\uACE0\uAE09 \uC124\uC815",
    server_url: "\uC11C\uBC84 URL",
    username: "\uC774\uBA54\uC77C",
    password: "\uBE44\uBC00\uBC88\uD638",
    password_confirm: "\uBE44\uBC00\uBC88\uD638 \uD655\uC778",
    pairing_code: "\uD398\uC5B4\uB9C1 \uCF54\uB4DC",
    pairing_code_optional: "\uD398\uC5B4\uB9C1 \uCF54\uB4DC (\uC120\uD0DD\uC0AC\uD56D, \uCF54\uB4DC \uC804\uC6A9 \uBAA8\uB4DC)",
    pair: "\uD398\uC5B4\uB9C1",
    pairing: "\uD398\uC5B4\uB9C1 \uC911...",
    connect: "\uC5F0\uACB0",
    connecting: "\uC5F0\uACB0 \uC911...",
    disconnect: "\uC5F0\uACB0 \uD574\uC81C",
    connection_status: "\uC5F0\uACB0 \uC0C1\uD0DC",
    connected: "\uC5F0\uACB0\uB428",
    disconnected: "\uC5F0\uACB0 \uC548 \uB428",
    token: "\uD1A0\uD070",
    health_check: "\uC0C1\uD0DC \uD655\uC778",
    checking: "\uD655\uC778 \uC911...",
    server_healthy: "\uC11C\uBC84\uAC00 \uC815\uC0C1\uC785\uB2C8\uB2E4",
    server_unreachable: "\uC11C\uBC84\uC5D0 \uC5F0\uACB0\uD560 \uC218 \uC5C6\uC2B5\uB2C8\uB2E4",
    pair_success: "\uC11C\uBC84\uC640 \uC131\uACF5\uC801\uC73C\uB85C \uD398\uC5B4\uB9C1\uB418\uC5C8\uC2B5\uB2C8\uB2E4",
    pair_failed: "\uD398\uC5B4\uB9C1 \uC2E4\uD328",
    language: "\uC5B8\uC5B4",
    no_chats: "\uC544\uC9C1 \uB300\uD654\uAC00 \uC5C6\uC2B5\uB2C8\uB2E4",
    welcome_title: "MoA\uC5D0 \uC624\uC2E0 \uAC83\uC744 \uD658\uC601\uD569\uB2C8\uB2E4",
    welcome_subtitle: "\uC790\uC728 AI \uC5B4\uC2DC\uC2A4\uD134\uD2B8",
    welcome_hint: "\uC544\uB798\uC5D0 \uBA54\uC2DC\uC9C0\uB97C \uC785\uB825\uD558\uC5EC \uB300\uD654\uB97C \uC2DC\uC791\uD558\uC138\uC694",
    delete_chat: "\uC0AD\uC81C",
    delete_confirm: "\uC774 \uB300\uD654\uB97C \uC0AD\uC81C\uD558\uC2DC\uACA0\uC2B5\uB2C8\uAE4C?",
    model: "\uBAA8\uB378",
    back_to_chat: "\uCC44\uD305\uC73C\uB85C \uB3CC\uC544\uAC00\uAE30",
    not_connected_hint: "\uCC44\uD305\uC744 \uC2DC\uC791\uD558\uB824\uBA74 \uB85C\uADF8\uC778\uD574\uC8FC\uC138\uC694",
    sync_status: "\uB3D9\uAE30\uD654 \uC0C1\uD0DC",
    sync_connected: "\uB3D9\uAE30\uD654 \uC5F0\uACB0\uB428",
    sync_disconnected: "\uB3D9\uAE30\uD654 \uC5F0\uACB0 \uC548 \uB428",
    sync_device_id: "\uB514\uBC14\uC774\uC2A4 ID",
    sync_trigger: "\uC804\uCCB4 \uB3D9\uAE30\uD654",
    sync_triggering: "\uB3D9\uAE30\uD654 \uC911...",
    sync_triggered: "\uC804\uCCB4 \uB3D9\uAE30\uD654\uAC00 \uC131\uACF5\uC801\uC73C\uB85C \uC2DC\uC791\uB418\uC5C8\uC2B5\uB2C8\uB2E4",
    sync_failed: "\uB3D9\uAE30\uD654 \uC2E4\uD328",
    platform: "\uD50C\uB7AB\uD3FC",
    // Auth flow
    login: "\uB85C\uADF8\uC778",
    login_title: "MoA \uB85C\uADF8\uC778",
    login_subtitle: "\uC790\uC728 AI \uC5B4\uC2DC\uC2A4\uD134\uD2B8",
    login_button: "\uB85C\uADF8\uC778",
    logging_in: "\uB85C\uADF8\uC778 \uC911...",
    login_failed: "\uB85C\uADF8\uC778\uC5D0 \uC2E4\uD328\uD558\uC600\uC2B5\uB2C8\uB2E4.",
    signup: "\uD68C\uC6D0\uAC00\uC785",
    signup_title: "\uD68C\uC6D0\uAC00\uC785",
    signup_subtitle: "MoA - Master of AI",
    signup_button: "\uACC4\uC815 \uB9CC\uB4E4\uAE30",
    signing_up: "\uACC4\uC815 \uC0DD\uC131 \uC911...",
    signup_success: "\uACC4\uC815\uC774 \uC0DD\uC131\uB418\uC5C8\uC2B5\uB2C8\uB2E4! \uB85C\uADF8\uC778\uD574\uC8FC\uC138\uC694.",
    signup_failed: "\uD68C\uC6D0\uAC00\uC785\uC5D0 \uC2E4\uD328\uD558\uC600\uC2B5\uB2C8\uB2E4.",
    no_account: "\uACC4\uC815\uC774 \uC5C6\uC73C\uC2E0\uAC00\uC694?",
    have_account: "\uC774\uBBF8 \uACC4\uC815\uC774 \uC788\uC73C\uC2E0\uAC00\uC694?",
    logout: "\uB85C\uADF8\uC544\uC6C3",
    logout_confirm: "\uB85C\uADF8\uC544\uC6C3 \uD558\uC2DC\uACA0\uC2B5\uB2C8\uAE4C?",
    // Interpreter
    interpreter: "통역",
    interpreter_idle: "대기",
    interpreter_connecting: "연결 중...",
    interpreter_ready: "연결됨",
    interpreter_listening: "듣는 중",
    interpreter_error: "오류",
    interpreter_stopping: "번역 중...",
    interpreter_start: "통역 시작",
    interpreter_stop: "중지",
    interpreter_bidirectional: "양방향",
    interpreter_hint: "시작 버튼을 눌러 실시간 음성 통역을 시작하세요",
    interpreter_listening_hint: "듣는 중... 말씀하세요",
    // Device selection
    select_device: "\uB514\uBC14\uC774\uC2A4 \uC120\uD0DD",
    select_device_subtitle: "\uC5F0\uACB0\uD560 MoA \uB514\uBC14\uC774\uC2A4\uB97C \uC120\uD0DD\uD558\uC138\uC694",
    device_online: "\uC628\uB77C\uC778",
    device_offline: "\uC624\uD504\uB77C\uC778",
    device_this: "\uC774 \uB514\uBC14\uC774\uC2A4",
    device_remote: "\uC6D0\uACA9",
    device_select: "\uC5F0\uACB0",
    device_no_devices: "\uB4F1\uB85D\uB41C \uB514\uBC14\uC774\uC2A4\uAC00 \uC5C6\uC2B5\uB2C8\uB2E4. \uC774 \uB514\uBC14\uC774\uC2A4\uAC00 \uC790\uB3D9\uC73C\uB85C \uB4F1\uB85D\uB429\uB2C8\uB2E4.",
    device_pairing_required: "\uC6D0\uACA9 \uB514\uBC14\uC774\uC2A4 \uC5F0\uACB0\uC5D0\uB294 \uD398\uC5B4\uB9C1 \uCF54\uB4DC\uAC00 \uD544\uC694\uD569\uB2C8\uB2E4",
    enter_pairing_code: "\uD398\uC5B4\uB9C1 \uCF54\uB4DC \uC785\uB825",
    verify_pairing: "\uD655\uC778",
    verifying: "\uD655\uC778 \uC911...",
    pairing_verified: "\uD398\uC5B4\uB9C1 \uD655\uC778 \uC644\uB8CC!",
    pairing_invalid: "\uD398\uC5B4\uB9C1 \uCF54\uB4DC\uAC00 \uC62C\uBC14\uB974\uC9C0 \uC54A\uC2B5\uB2C8\uB2E4",
    auto_connecting: "\uC790\uB3D9 \uC5F0\uACB0 \uC911...",
    // Device management in settings
    my_devices: "\uB0B4 \uB514\uBC14\uC774\uC2A4",
    device_name: "\uB514\uBC14\uC774\uC2A4 \uC774\uB984",
    device_platform: "\uD50C\uB7AB\uD3FC",
    device_last_seen: "\uB9C8\uC9C0\uB9C9 \uC811\uC18D",
    set_pairing_code: "\uD398\uC5B4\uB9C1 \uCF54\uB4DC \uC124\uC815",
    change_pairing_code: "\uD398\uC5B4\uB9C1 \uCF54\uB4DC \uBCC0\uACBD",
    remove_pairing_code: "\uD398\uC5B4\uB9C1 \uCF54\uB4DC \uC81C\uAC70",
    pairing_code_set: "\uD398\uC5B4\uB9C1 \uCF54\uB4DC\uAC00 \uC5C5\uB370\uC774\uD2B8\uB418\uC5C8\uC2B5\uB2C8\uB2E4",
    pairing_code_removed: "\uD398\uC5B4\uB9C1 \uCF54\uB4DC\uAC00 \uC81C\uAC70\uB418\uC5C8\uC2B5\uB2C8\uB2E4",
    new_pairing_code: "\uC0C8 \uD398\uC5B4\uB9C1 \uCF54\uB4DC",
    save_pairing_code: "\uC800\uC7A5",
    account_info: "\uACC4\uC815 \uC815\uBCF4",
    // API Keys & Credits
    api_keys: "API \uD0A4",
    api_key_claude: "Claude (Anthropic)",
    api_key_openai: "OpenAI",
    api_key_gemini: "Gemini (Google)",
    api_key_placeholder: "sk-... \uB610\uB294 AIza...",
    api_key_saved: "API \uD0A4\uAC00 \uC800\uC7A5\uB418\uC5C8\uC2B5\uB2C8\uB2E4",
    api_key_cleared: "API \uD0A4\uAC00 \uC81C\uAC70\uB418\uC5C8\uC2B5\uB2C8\uB2E4",
    api_key_hint: "\uC790\uC2E0\uC758 \uD0A4\uB97C \uC0AC\uC6A9\uD558\uBA74 \uBB34\uB8CC\uC785\uB2C8\uB2E4. \uD0A4 \uC5C6\uC774 \uC0AC\uC6A9 \uC2DC \uC6B4\uC601\uC790 \uD06C\uB808\uB527 \uCC28\uAC10 (2\uBC30).",
    credits: "\uD06C\uB808\uB527",
    credit_balance: "\uC794\uC561",
    credit_buy: "\uD06C\uB808\uB527 \uCDA9\uC804",
    credit_history: "\uC774\uC6A9 \uB0B4\uC5ED",
    credit_package_basic: "\uBCA0\uC774\uC9C1",
    credit_package_standard: "\uC2A4\uD0E0\uB2E4\uB4DC",
    credit_package_premium: "\uD504\uB9AC\uBBF8\uC5C4",
    credit_package_pro: "\uD504\uB85C",
    credit_operator_hint: "\uC790\uC2E0\uC758 API \uD0A4\uAC00 \uC5C6\uC744 \uB54C \uD06C\uB808\uB527\uC774 \uC0AC\uC6A9\uB429\uB2C8\uB2E4 (\uC2E4\uC81C \uBE44\uC6A9\uC758 2\uBC30).",
    // LLM / Model selection
    llm_settings: "AI \uBAA8\uB378",
    llm_provider: "\uC81C\uACF5\uC5C5\uCCB4",
    llm_model: "\uBAA8\uB378",
    llm_provider_claude: "Claude (Anthropic)",
    llm_provider_openai: "OpenAI",
    llm_provider_gemini: "Gemini (Google)",
    connection_mode: "\uC5F0\uACB0 \uBC29\uC2DD",
    connection_mode_relay: "\uD074\uB77C\uC6B0\uB4DC (MoA \uB9B4\uB808\uC774)",
    connection_mode_local: "\uB85C\uCEEC \uAC8C\uC774\uD2B8\uC6E8\uC774",
    connection_mode_relay_hint: "MoA \uB9B4\uB808\uC774 \uC11C\uBC84\uB97C \uD1B5\uD574 \uC5F0\uACB0\uB429\uB2C8\uB2E4. \uC694\uCCAD\uB2F9 \uD06C\uB808\uB527\uC774 \uCC28\uAC10\uB429\uB2C8\uB2E4.",
    connection_mode_local_hint: "\uC790\uCCB4 API \uD0A4\uB97C \uC0AC\uC6A9\uD558\uC5EC \uB85C\uCEEC \uAC8C\uC774\uD2B8\uC6E8\uC774\uB85C \uC5F0\uACB0\uB429\uB2C8\uB2E4.",
    advanced_settings_toggle: "\uACE0\uAE09 \uC124\uC815",
    local_gateway_url: "\uB85C\uCEEC \uAC8C\uC774\uD2B8\uC6E8\uC774 URL",
    // Sidebar sections
    sidebar_devices: "\uB514\uBC14\uC774\uC2A4",
    sidebar_channels: "\uCC44\uB110",
    sidebar_tools: "\uB3C4\uAD6C",
    sidebar_no_devices: "\uB514\uBC14\uC774\uC2A4 \uC5C6\uC74C",
    sidebar_no_channels: "\uCC44\uB110 \uC5C6\uC74C",
    sidebar_no_tools: "\uB3C4\uAD6C \uC5C6\uC74C",
    sidebar_chats: "\uB300\uD654",
    greeting_prompt: "[SYSTEM] 사용자가 처음 로그인했습니다. 아이디는 \"{username}\"입니다. 당신은 MoA, 사용자의 개인 AI 비서입니다. 정중하고 조심스러운 태도로 여러 단계에 걸쳐 대화를 시작하세요. 하나의 응답 안에서 다음 순서로 구성하세요:\n1. 먼저 따뜻하게 자기소개: \"안녕하세요. 저는 MoA라고 합니다. 간단하게 저를 소개하면 사용자님의 개인 AI 비서입니다. 앞으로 잘 부탁드립니다.\"\n2. 그 다음, 조심스럽게 이름과 직업을 여쭤보기: \"혹시 사용자님의 이름과 직업을 여쭈어봐도 될까요?\"\n3. 마지막으로, 호칭을 어떻게 하면 좋을지 정중하게 묻기: \"실례가 되지 않는다면 제가 사용자님을 어떤 호칭으로 부르는 것이 좋을까요?\"\n위 내용을 참고하되 자연스럽게 변형하여 말하세요. 처음 만나는 중요한 분을 대하듯 공손하고 예의바르게 대화하세요. 지나치게 캐주얼하지 않게, 따뜻하지만 전문적인 비서의 톤을 유지하세요. 반드시 한국어로 대화하세요.",
    greeting_prompt_returning: "[SYSTEM] \uC0AC\uC6A9\uC790 \"{username}\"\uB2D8\uC774 \uB85C\uADF8\uC778\uD588\uC2B5\uB2C8\uB2E4. \uB2F9\uC2E0\uC740 \uAC1C\uC778 AI \uBE44\uC11C MoA\uC785\uB2C8\uB2E4. \uC0AC\uC6A9\uC790\uB97C \uC798 \uC544\uB294 \uBE44\uC11C\uCC98\uB7FC \uC778\uC0AC\uD558\uC138\uC694. \uAE30\uC5B5\uD558\uACE0 \uC788\uB294 \uC815\uBCF4(\uC774\uB984, \uC9C1\uC5C5, \uC120\uD638\uB3C4)\uB97C \uD65C\uC6A9\uD558\uC138\uC694. \uB530\uB73B\uD558\uACE0 \uAC04\uACB0\uD558\uAC8C, \uC624\uB298 \uBB34\uC5C7\uC744 \uB3C4\uC640\uB4DC\uB9B4\uC9C0 \uBB3C\uC5B4\uBD10\uC8FC\uC138\uC694. \uD55C\uAD6D\uC5B4\uB85C \uB300\uD654\uD558\uC138\uC694.",
    // Lock screen
    lock_title: "MoA \uc7a0\uae08",
    lock_subtitle: "\ube44\ubc00\ubc88\ud638\ub97c \uc785\ub825\ud558\uc5ec \uc7a0\uae08\uc744 \ud574\uc81c\ud558\uc138\uc694",
    lock_password_placeholder: "\ube44\ubc00\ubc88\ud638",
    lock_unlock: "\uc7a0\uae08 \ud574\uc81c",
    lock_unlocking: "\ud655\uc778 \uc911...",
    lock_failed: "\ube44\ubc00\ubc88\ud638\uac00 \uc62c\ubc14\ub974\uc9c0 \uc54a\uc2b5\ub2c8\ub2e4.",
    lock_logout: "\uacc4\uc815 \uc804\ud658",
    connect_folder: "\ud3f4\ub354 \uc5f0\uacb0",
    connect_github: "GitHub \uc5f0\uacb0",
    connect_folder_hint: "\ub85c\uceec \ud3f4\ub354\ub97c \uc791\uc5c5 \ud3f4\ub354\ub85c \uc5f0\uacb0",
    connect_github_hint: "GitHub \uc800\uc7a5\uc18c\ub97c \ud074\ub860\ud558\uc5ec \uc791\uc5c5 \ud3f4\ub354\ub85c \uc124\uc815",
    connect_github_placeholder: "https://github.com/user/repo",
    workspace_connected: "\uc791\uc5c5 \ud3f4\ub354\uac00 \uc5f0\uacb0\ub418\uc5c8\uc2b5\ub2c8\ub2e4",
    repo_cloning: "\uc800\uc7a5\uc18c \ud074\ub860 \uc911...",
  },
};

export function t(key: TranslationKey, locale: Locale): string {
  return translations[locale]?.[key] ?? translations.en[key] ?? key;
}
