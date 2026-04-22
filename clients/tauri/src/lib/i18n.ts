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
  | "remove_device"
  | "confirm_remove_device"
  | "device_removed"
  | "pairing_code_set"
  | "pairing_code_removed"
  | "new_pairing_code"
  | "save_pairing_code"
  | "switch_to_relay"
  | "confirm_switch_to_relay"
  | "switched_to_relay"
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
  | "greeting_first_chat_local"
  // Local-LLM first-launch bootstrap screen
  | "local_llm_bootstrap_title"
  | "local_llm_bootstrap_subtitle"
  | "local_llm_bootstrap_stage_not_started"
  | "local_llm_bootstrap_stage_skipped"
  | "local_llm_bootstrap_stage_probing"
  | "local_llm_bootstrap_stage_checking_disk"
  | "local_llm_bootstrap_stage_installing_ollama"
  | "local_llm_bootstrap_stage_waiting_for_daemon"
  | "local_llm_bootstrap_stage_pulling_model"
  | "local_llm_bootstrap_stage_persisting"
  | "local_llm_bootstrap_stage_done"
  | "local_llm_bootstrap_stage_error"
  | "local_llm_bootstrap_retry"
  | "local_llm_bootstrap_poll_retry"
  | "local_llm_bootstrap_skip"
  // Sidebar gun-metaphor labels
  | "sidebar_base_gun_heading"
  | "sidebar_option_gun_heading"
  | "sidebar_badge_base_gun"
  | "sidebar_badge_byok"
  | "sidebar_badge_credit"
  // Billing page + low-balance banner
  | "billing_title"
  | "billing_balance"
  | "billing_credits_unit"
  | "billing_fx_rate"
  | "billing_subscription"
  | "billing_sub_active"
  | "billing_sub_renewal"
  | "billing_cancel_subscription"
  | "billing_subscribe_success"
  | "billing_cancel_success"
  | "billing_interval_month"
  | "billing_interval_year"
  | "billing_topup"
  | "billing_checkout_failed"
  | "billing_alerts"
  | "billing_low_balance_label"
  | "billing_auto_recharge_label"
  | "billing_auto_amount_label"
  | "billing_auto_threshold_label"
  | "billing_auto_card_note"
  | "low_balance_banner_text"
  | "low_balance_recharge"
  | "sidebar_billing_link"
  | "billing_subscribe_pending"
  | "billing_cancel_with_refund"
  // Pending auto-recharge modal
  | "pending_ar_title"
  | "pending_ar_body"
  | "pending_ar_timer"
  | "pending_ar_approve"
  | "pending_ar_defer"
  | "pending_ar_cancel"
  | "pending_ar_charge_ok"
  | "pending_ar_charge_skipped"
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
  | "repo_cloning"
  // ── Settings → Local LLM (Gemma 4) panel ──
  | "local_llm_section_title"
  | "local_llm_loading"
  | "local_llm_current_model"
  | "local_llm_not_installed"
  | "local_llm_status_ok"
  | "local_llm_status_pull_pending"
  | "local_llm_daemon_down"
  | "local_llm_change_tier"
  | "local_llm_pick_tier"
  | "local_llm_mobile_cap"
  | "local_llm_insufficient_vram"
  | "local_llm_audio_native"
  | "local_llm_hardware_summary"
  | "local_llm_hardware_unknown"
  | "local_llm_ram"
  | "local_llm_disk_free"
  | "local_llm_reprobe"
  | "local_llm_reprobing"
  | "local_llm_auto_upgrade_notify"
  | "local_llm_offline_only"
  | "local_llm_uninstall_label"
  | "local_llm_uninstall"
  | "local_llm_uninstalling"
  | "local_llm_uninstall_confirm"
  | "local_llm_installed_inventory"
  | "local_llm_switch_manual_hint"
  | "local_llm_switch_confirm"
  | "local_llm_switching"
  | "local_llm_switch_attempt"
  // ── Secretary migrator (§11.3 / PR #10) ──
  | "secretary_migrator_title"
  | "secretary_migrator_intro"
  | "secretary_migrator_current"
  | "secretary_migrator_recommended"
  | "secretary_migrator_reason"
  | "secretary_migrator_accept"
  | "secretary_migrator_saved"
  | "secretary_migrator_no_typecast_voice"
  | "secretary_engine_cosyvoice"
  | "secretary_engine_kokoro";

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
    server_unreachable: "Server is unreachable. Please install MoA on your computer: https://mymoa.app",
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
    remove_device: "Remove Device",
    confirm_remove_device: "Are you sure you want to remove this device?",
    device_removed: "Device removed",
    pairing_code_set: "Pairing code updated",
    pairing_code_removed: "Pairing code removed",
    new_pairing_code: "New pairing code",
    save_pairing_code: "Save",
    switch_to_relay: "Switch to Server Relay",
    confirm_switch_to_relay: "Remove all LLM API keys and switch to server relay mode? (Credits will be charged)",
    switched_to_relay: "Switched to server relay mode. Credits will be used for LLM calls.",
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
    greeting_first_chat_local: "Hello. I am MoA, your friendly AI assistant.\n\nI'm a local AI that works on your device even without an internet connection. For complex problems that need advanced reasoning, I'll call on heavyweight LLMs such as Claude, Gemini, or GPT as optional high-performance upgrades.\n\nUsing those advanced models consumes credits. If you already have an API key for one of them, you can paste it into Settings and I'll use your key instead — no credits will be spent in that case.",
    local_llm_bootstrap_title: "Loading your local brain — Gemma 4",
    local_llm_bootstrap_subtitle: "One-time setup. We are installing the local brain so MoA can think offline.",
    local_llm_bootstrap_stage_not_started: "Waiting for the backend to start…",
    local_llm_bootstrap_stage_skipped: "Local install skipped — using cloud fallback.",
    local_llm_bootstrap_stage_probing: "Probing hardware…",
    local_llm_bootstrap_stage_checking_disk: "Checking free disk space…",
    local_llm_bootstrap_stage_installing_ollama: "Installing Ollama runtime…",
    local_llm_bootstrap_stage_waiting_for_daemon: "Waiting for Ollama daemon…",
    local_llm_bootstrap_stage_pulling_model: "Downloading Gemma 4 model…",
    local_llm_bootstrap_stage_persisting: "Saving local-LLM configuration…",
    local_llm_bootstrap_stage_done: "Ready — Gemma 4 is loaded.",
    local_llm_bootstrap_stage_error: "Setup failed — you can still use a cloud LLM from Settings.",
    local_llm_bootstrap_retry: "(retry #{n})",
    local_llm_bootstrap_poll_retry: "Backend not responding — retrying…",
    local_llm_bootstrap_skip: "Continue without local AI",
    sidebar_base_gun_heading: "Base brain (always loaded)",
    sidebar_option_gun_heading: "Optional brains (swap in)",
    sidebar_badge_base_gun: "local",
    sidebar_badge_byok: "BYOK",
    sidebar_badge_credit: "credits",
    billing_title: "Billing",
    billing_balance: "Credit balance",
    billing_credits_unit: "credits",
    billing_fx_rate: "1 USD ≈ {rate} KRW",
    billing_subscription: "Subscription",
    billing_sub_active: "Active: {plan}",
    billing_sub_renewal: "Next renewal: {date}",
    billing_cancel_subscription: "Cancel subscription",
    billing_subscribe_success: "Subscribed to {plan}.",
    billing_cancel_success: "Subscription cancelled.",
    billing_interval_month: "month",
    billing_interval_year: "year",
    billing_topup: "One-time top-up",
    billing_checkout_failed: "Could not open checkout — try again later.",
    billing_alerts: "Alerts & auto-recharge",
    billing_low_balance_label: "Warn me when credits fall below:",
    billing_auto_recharge_label: "Enable auto-recharge (uses saved card)",
    billing_auto_amount_label: "Auto-recharge amount:",
    billing_auto_threshold_label: "Trigger when credits fall below:",
    billing_auto_card_note: "Your card is saved during your next top-up checkout — tick 'Save card' there.",
    low_balance_banner_text: "{balance} credits left (threshold {threshold}). Time to recharge.",
    low_balance_recharge: "Recharge",
    sidebar_billing_link: "Billing & credits",
    billing_subscribe_pending: "Stripe checkout opened for {plan}. Credits land after payment completes.",
    billing_cancel_with_refund: "Subscription cancelled. Refund of {amount} processed via Stripe.",
    pending_ar_title: "Auto-recharge confirmation",
    pending_ar_body: "Your credits dropped to {balance} (threshold {threshold}). Charge {amount} to your saved card?",
    pending_ar_timer: "Expires in {m}:{s}",
    pending_ar_approve: "Charge now",
    pending_ar_defer: "Remind me later",
    pending_ar_cancel: "Cancel",
    pending_ar_charge_ok: "Credits recharged.",
    pending_ar_charge_skipped: "Charge skipped — {reason}. You can try a manual top-up.",
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
    local_llm_section_title: "Local AI model (Gemma 4)",
    local_llm_loading: "Loading local-LLM status…",
    local_llm_current_model: "Installed model",
    local_llm_not_installed: "Not installed yet",
    local_llm_status_ok: "Ready",
    local_llm_status_pull_pending: "Needs pull",
    local_llm_daemon_down: "Ollama daemon offline",
    local_llm_change_tier: "Change tier",
    local_llm_pick_tier: "Pick a tier…",
    local_llm_mobile_cap: "Not available on mobile",
    local_llm_insufficient_vram: "Needs ~{need} GB VRAM",
    local_llm_audio_native: "native audio",
    local_llm_hardware_summary: "Detected hardware",
    local_llm_hardware_unknown: "Unknown — run a hardware probe",
    local_llm_ram: "RAM",
    local_llm_disk_free: "Disk free",
    local_llm_reprobe: "Re-scan hardware",
    local_llm_reprobing: "Scanning hardware…",
    local_llm_auto_upgrade_notify: "Notify me when better hardware unlocks a larger tier",
    local_llm_offline_only: "Offline-only mode (never call the cloud even with an API key)",
    local_llm_uninstall_label: "Remove the local model from disk",
    local_llm_uninstall: "Uninstall",
    local_llm_uninstalling: "Uninstalling…",
    local_llm_uninstall_confirm: "Remove the local Gemma 4 model? You can reinstall later from this screen.",
    local_llm_installed_inventory: "Installed models ({count})",
    local_llm_switch_manual_hint: "Run this command to switch tiers:\n\n{command}",
    local_llm_switch_confirm: "Switch local Gemma 4 to tier {tier}? This downloads the new model and can take several minutes.",
    local_llm_switching: "Switching tier…",
    local_llm_switch_attempt: "attempt {n}",
    secretary_migrator_title: "Secretary migration (offline)",
    secretary_migrator_intro:
      "If you go offline, we'll swap your paid Typecast secretary for the closest offline voice so the chat UX stays the same.",
    secretary_migrator_current: "Current Typecast secretary",
    secretary_migrator_recommended: "Recommended offline replacement",
    secretary_migrator_reason: "Why this match",
    secretary_migrator_accept: "Use this offline secretary",
    secretary_migrator_saved: "Saved — we'll use this voice when you're offline",
    secretary_migrator_no_typecast_voice: "No Typecast voice selected yet",
    secretary_engine_cosyvoice: "Offline Pro · CosyVoice 2",
    secretary_engine_kokoro: "Offline Basic · Kokoro",
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
    server_unreachable: "\uC11C\uBC84\uC5D0 \uC5F0\uACB0\uD560 \uC218 \uC5C6\uC2B5\uB2C8\uB2E4. \uB85C\uCEEC \uCEF4\uD4E8\uD130\uC5D0 MoA \uC571\uC744 \uC124\uCE58\uD574\uC8FC\uC138\uC694: https://mymoa.app",
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
    remove_device: "\uB514\uBC14\uC774\uC2A4 \uC0AD\uC81C",
    confirm_remove_device: "\uC774 \uB514\uBC14\uC774\uC2A4\uB97C \uC0AD\uC81C\uD558\uC2DC\uACA0\uC2B5\uB2C8\uAE4C?",
    device_removed: "\uB514\uBC14\uC774\uC2A4\uAC00 \uC0AD\uC81C\uB418\uC5C8\uC2B5\uB2C8\uB2E4",
    pairing_code_set: "\uD398\uC5B4\uB9C1 \uCF54\uB4DC\uAC00 \uC5C5\uB370\uC774\uD2B8\uB418\uC5C8\uC2B5\uB2C8\uB2E4",
    pairing_code_removed: "\uD398\uC5B4\uB9C1 \uCF54\uB4DC\uAC00 \uC81C\uAC70\uB418\uC5C8\uC2B5\uB2C8\uB2E4",
    new_pairing_code: "\uC0C8 \uD398\uC5B4\uB9C1 \uCF54\uB4DC",
    save_pairing_code: "\uC800\uC7A5",
    switch_to_relay: "\uC11C\uBC84 \uACBD\uC720\uB85C \uBCC0\uACBD",
    confirm_switch_to_relay: "LLM API key\uB97C \uBAA8\uB450 \uC81C\uAC70\uD558\uACE0 \uC11C\uBC84 \uACBD\uC720 \uBAA8\uB4DC\uB85C \uC804\uD658\uD558\uC2DC\uACA0\uC2B5\uB2C8\uAE4C? (\uD06C\uB808\uB527 \uCC28\uAC10)",
    switched_to_relay: "\uC11C\uBC84 \uACBD\uC720 \uBAA8\uB4DC\uB85C \uC804\uD658\uB418\uC5C8\uC2B5\uB2C8\uB2E4. LLM \uD638\uCD9C \uC2DC \uD06C\uB808\uB527\uC774 \uCC28\uAC10\uB429\uB2C8\uB2E4.",
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
    greeting_first_chat_local: "안녕하세요. 저는 당신의 친절한 AI 비서 MoA입니다.\n\n저는 로컬에서 인터넷 연결 없이도 작동하는 AI입니다만, 고급 추론이 필요한 복잡한 문제 해결을 위해서 특별히 클로드나 제미나이, 또는 GPT와 같은 LLM 모델을 옵션 총(Gun)처럼 연결하여 사용할 예정입니다.\n\n고급 LLM 모델을 문제 해결에 사용할 때에는 크레딧이 소진됩니다. 다만 사용자님께서 위 LLM 모델의 API key를 가지고 계신다면 설정 화면에 입력해 주시면, 그 키가 쓰이는 동안에는 별도의 크레딧이 소진되지 않습니다.",
    local_llm_bootstrap_title: "로컬 브레인을 장착하고 있어요 — Gemma 4",
    local_llm_bootstrap_subtitle: "최초 1회 설치입니다. 오프라인에서도 작동하는 로컬 두뇌를 준비하고 있어요.",
    local_llm_bootstrap_stage_not_started: "백엔드 기동을 기다리는 중…",
    local_llm_bootstrap_stage_skipped: "로컬 설치를 건너뛰었습니다 — 클라우드로만 동작합니다.",
    local_llm_bootstrap_stage_probing: "하드웨어를 확인하는 중…",
    local_llm_bootstrap_stage_checking_disk: "저장 공간을 확인하는 중…",
    local_llm_bootstrap_stage_installing_ollama: "Ollama 런타임을 설치하는 중…",
    local_llm_bootstrap_stage_waiting_for_daemon: "Ollama 데몬을 기다리는 중…",
    local_llm_bootstrap_stage_pulling_model: "Gemma 4 모델을 내려받는 중…",
    local_llm_bootstrap_stage_persisting: "로컬 LLM 설정을 저장하는 중…",
    local_llm_bootstrap_stage_done: "준비 완료 — Gemma 4가 장착되었습니다.",
    local_llm_bootstrap_stage_error: "설치에 실패했어요 — 설정에서 클라우드 LLM을 사용할 수 있어요.",
    local_llm_bootstrap_retry: "(재시도 {n}회차)",
    local_llm_bootstrap_poll_retry: "서버 응답 대기 중 — 재시도합니다…",
    local_llm_bootstrap_skip: "로컬 AI 없이 계속하기",
    sidebar_base_gun_heading: "기본 브레인 (항상 장착)",
    sidebar_option_gun_heading: "옵션 브레인 (갈아끼우기)",
    sidebar_badge_base_gun: "로컬",
    sidebar_badge_byok: "BYOK",
    sidebar_badge_credit: "크레딧",
    billing_title: "결제 및 크레딧",
    billing_balance: "크레딧 잔액",
    billing_credits_unit: "크레딧",
    billing_fx_rate: "1달러 ≈ {rate}원",
    billing_subscription: "구독",
    billing_sub_active: "이용 중: {plan}",
    billing_sub_renewal: "다음 갱신일: {date}",
    billing_cancel_subscription: "구독 해지",
    billing_subscribe_success: "{plan} 구독이 시작되었습니다.",
    billing_cancel_success: "구독이 해지되었습니다.",
    billing_interval_month: "월",
    billing_interval_year: "년",
    billing_topup: "일회성 충전",
    billing_checkout_failed: "결제창을 열 수 없습니다. 잠시 후 다시 시도해 주세요.",
    billing_alerts: "알림 및 자동 충전",
    billing_low_balance_label: "크레딧이 이 값 이하로 내려가면 알려주세요:",
    billing_auto_recharge_label: "자동 충전 사용 (저장된 카드로 결제)",
    billing_auto_amount_label: "자동 충전 금액:",
    billing_auto_threshold_label: "자동 충전 조건 (이 값 이하일 때):",
    billing_auto_card_note: "다음 수동 충전 시 '카드 저장' 체크박스를 선택하시면 이 자동 충전에 사용할 카드가 저장됩니다.",
    low_balance_banner_text: "남은 크레딧: {balance} (임계값 {threshold}). 충전할 시점입니다.",
    low_balance_recharge: "충전하기",
    sidebar_billing_link: "결제 및 크레딧",
    billing_subscribe_pending: "{plan} 결제창(Stripe)을 열었습니다. 결제 완료 후 크레딧이 지급됩니다.",
    billing_cancel_with_refund: "구독이 해지되었습니다. Stripe에서 {amount} 환불 처리되었습니다.",
    pending_ar_title: "자동 충전 확인",
    pending_ar_body: "남은 크레딧이 {balance}로 떨어졌어요 (임계값 {threshold}). 저장된 카드로 {amount}을(를) 결제할까요?",
    pending_ar_timer: "{m}분 {s}초 후 자동 취소",
    pending_ar_approve: "지금 결제",
    pending_ar_defer: "나중에 다시 알림",
    pending_ar_cancel: "취소",
    pending_ar_charge_ok: "크레딧이 충전되었습니다.",
    pending_ar_charge_skipped: "결제 건너뜀 — {reason}. 수동 충전을 시도해 주세요.",
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
    local_llm_section_title: "로컬 AI 모델 (Gemma 4)",
    local_llm_loading: "로컬 AI 상태를 불러오는 중…",
    local_llm_current_model: "설치된 모델",
    local_llm_not_installed: "아직 설치되지 않았습니다",
    local_llm_status_ok: "정상",
    local_llm_status_pull_pending: "다운로드 필요",
    local_llm_daemon_down: "Ollama 데몬이 실행되고 있지 않습니다",
    local_llm_change_tier: "모델 티어 변경",
    local_llm_pick_tier: "티어를 선택하세요…",
    local_llm_mobile_cap: "모바일에서는 사용할 수 없습니다",
    local_llm_insufficient_vram: "약 {need} GB의 VRAM/메모리가 필요합니다",
    local_llm_audio_native: "음성 입력 지원",
    local_llm_hardware_summary: "감지된 하드웨어",
    local_llm_hardware_unknown: "미확인 — 하드웨어 재검사를 실행하세요",
    local_llm_ram: "RAM",
    local_llm_disk_free: "디스크 여유",
    local_llm_reprobe: "하드웨어 재검사",
    local_llm_reprobing: "하드웨어를 검사하는 중…",
    local_llm_auto_upgrade_notify: "하드웨어가 향상되면 더 큰 모델로 업그레이드를 알림",
    local_llm_offline_only: "오프라인 전용 모드 (API 키가 있어도 클라우드를 호출하지 않음)",
    local_llm_uninstall_label: "로컬 모델을 디스크에서 제거",
    local_llm_uninstall: "제거",
    local_llm_uninstalling: "제거 중…",
    local_llm_uninstall_confirm: "로컬 Gemma 4 모델을 제거할까요? 이 화면에서 언제든 다시 설치할 수 있습니다.",
    local_llm_installed_inventory: "설치된 모델 ({count}개)",
    local_llm_switch_manual_hint: "다음 명령을 실행해 티어를 변경하세요:\n\n{command}",
    local_llm_switch_confirm: "로컬 Gemma 4를 {tier} 티어로 변경할까요? 새 모델을 다운로드하며 몇 분이 걸릴 수 있습니다.",
    local_llm_switching: "티어 변경 중…",
    local_llm_switch_attempt: "{n}번째 시도",
    secretary_migrator_title: "오프라인 비서 전환",
    secretary_migrator_intro:
      "오프라인 전환 시 평소 쓰시던 Typecast 비서와 가장 비슷한 오프라인 음성을 자동으로 추천합니다. 브랜드 UX는 그대로 유지됩니다.",
    secretary_migrator_current: "현재 Typecast 비서",
    secretary_migrator_recommended: "추천 오프라인 비서",
    secretary_migrator_reason: "이 비서를 고른 이유",
    secretary_migrator_accept: "이 오프라인 비서 사용",
    secretary_migrator_saved: "저장되었습니다 — 오프라인 시 이 음성을 사용합니다",
    secretary_migrator_no_typecast_voice: "아직 선택된 Typecast 비서가 없습니다",
    secretary_engine_cosyvoice: "오프라인 프로 · CosyVoice 2",
    secretary_engine_kokoro: "오프라인 기본 · Kokoro",
  },
};

export function t(key: TranslationKey, locale: Locale): string {
  return translations[locale]?.[key] ?? translations.en[key] ?? key;
}
