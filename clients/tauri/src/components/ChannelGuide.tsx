import { useState } from "react";
import type { Locale } from "../lib/i18n";
import { saveChannelConfig } from "../lib/tauri-bridge";

interface ChannelGuideProps {
  channelName: string;
  locale: Locale;
  onClose: () => void;
}

interface GuideContent {
  title: string;
  titleKo: string;
  steps: string[];
  stepsKo: string[];
  configExample: string;
  /** Input fields shown as a simple form — user fills these instead of editing TOML. */
  inputFields?: InputFieldDef[];
}

interface InputFieldDef {
  /** TOML key (e.g. "bot_token") */
  key: string;
  /** Display label */
  label: string;
  labelKo: string;
  /** Placeholder text */
  placeholder: string;
  /** If true, renders as a multi-value comma-separated field → saved as JSON array */
  isArray?: boolean;
  /** If true, this field is required */
  required?: boolean;
}

const CHANNEL_GUIDES: Record<string, GuideContent> = {
  telegram: {
    title: "Telegram Bot Setup",
    titleKo: "텔레그램 봇 설정",
    steps: [
      "Open Telegram and search for @BotFather.",
      "Send /newbot and follow the prompts to create a bot.",
      "BotFather will give you a Bot Token. Copy it and paste below.",
      "To find your User ID: search for @userinfobot and send /start.",
    ],
    stepsKo: [
      "텔레그램을 열고 @BotFather를 검색하세요.",
      "/newbot 을 보내고 안내에 따라 봇을 만드세요.",
      "BotFather가 알려주는 Bot Token을 아래에 붙여넣으세요.",
      "내 User ID 확인: @userinfobot 에게 /start를 보내면 알 수 있습니다.",
    ],
    configExample: `[channels.telegram]\nbot_token = "YOUR_BOT_TOKEN"\nallowed_users = ["YOUR_USER_ID"]`,
    inputFields: [
      {
        key: "bot_token",
        label: "Bot Token",
        labelKo: "Bot Token (봇 토큰)",
        placeholder: "123456:ABC-DEF1234ghIkl-zyx57W2v...",
        required: true,
      },
      {
        key: "allowed_users",
        label: "Your User ID (from @userinfobot)",
        labelKo: "내 User ID (@userinfobot에서 확인)",
        placeholder: "123456789",
        isArray: true,
      },
    ],
  },

  discord: {
    title: "Discord Bot Setup",
    titleKo: "디스코드 봇 설정",
    steps: [
      "Go to discord.com/developers/applications → New Application.",
      "Go to Bot → Reset Token → copy the Bot Token.",
      "Enable 'Message Content Intent' under Privileged Gateway Intents.",
      "OAuth2 → URL Generator → select 'bot' + 'Send Messages' → invite bot.",
      "Paste the Bot Token and your User ID below.",
    ],
    stepsKo: [
      "discord.com/developers/applications → New Application을 클릭하세요.",
      "Bot → Reset Token → Bot Token을 복사하세요.",
      "Privileged Gateway Intents에서 'Message Content Intent'를 활성화하세요.",
      "OAuth2 → URL Generator → 'bot' + 'Send Messages' 선택 → 봇 초대.",
      "아래에 Bot Token과 User ID를 입력하세요.",
    ],
    configExample: `[channels.discord]\nbot_token = "YOUR_BOT_TOKEN"\nallowed_users = ["YOUR_USER_ID"]`,
    inputFields: [
      {
        key: "bot_token",
        label: "Bot Token",
        labelKo: "Bot Token (봇 토큰)",
        placeholder: "MTIz...",
        required: true,
      },
      {
        key: "allowed_users",
        label: "Your User ID",
        labelKo: "내 User ID",
        placeholder: "123456789012345678",
        isArray: true,
      },
    ],
  },

  slack: {
    title: "Slack Bot Setup",
    titleKo: "슬랙 봇 설정",
    steps: [
      "Go to api.slack.com/apps → Create New App.",
      "Add Bot Token Scopes: chat:write, channels:history, im:history.",
      "Install to Workspace and copy the Bot User OAuth Token.",
      "Paste below.",
    ],
    stepsKo: [
      "api.slack.com/apps → Create New App을 클릭하세요.",
      "Bot Token Scopes 추가: chat:write, channels:history, im:history.",
      "워크스페이스에 설치하고 Bot User OAuth Token을 복사하세요.",
      "아래에 붙여넣으세요.",
    ],
    configExample: `[channels.slack]\nbot_token = "xoxb-..."\napp_token = "xapp-..."`,
    inputFields: [
      {
        key: "bot_token",
        label: "Bot User OAuth Token",
        labelKo: "Bot User OAuth Token",
        placeholder: "xoxb-...",
        required: true,
      },
      {
        key: "app_token",
        label: "App-Level Token",
        labelKo: "App-Level Token",
        placeholder: "xapp-...",
      },
    ],
  },

  kakao: {
    title: "KakaoTalk Channel Setup",
    titleKo: "카카오톡 채널 설정",
    steps: [
      "Go to developers.kakao.com → Create Application.",
      "Copy the REST API Key and Admin Key.",
      "Set up a Kakao Channel and configure the webhook URL.",
      "Paste your keys below.",
    ],
    stepsKo: [
      "developers.kakao.com → 애플리케이션 추가하기를 클릭하세요.",
      "REST API 키와 Admin 키를 복사하세요.",
      "카카오톡 채널을 만들고 웹훅 URL을 설정하세요.",
      "아래에 키를 입력하세요.",
    ],
    configExample: `[channels.kakao]\nrest_api_key = "..."\nadmin_key = "..."`,
    inputFields: [
      {
        key: "rest_api_key",
        label: "REST API Key",
        labelKo: "REST API Key",
        placeholder: "abcdef1234567890...",
        required: true,
      },
      {
        key: "admin_key",
        label: "Admin Key",
        labelKo: "Admin Key",
        placeholder: "abcdef1234567890...",
        required: true,
      },
    ],
  },

  // Channels without input fields — show config example only
  matrix: {
    title: "Matrix Setup",
    titleKo: "Matrix 설정 안내",
    steps: [
      "Set up a Matrix bot account and get an access token.",
      "Add the config below to config.toml.",
    ],
    stepsKo: [
      "Matrix 봇 계정을 만들고 접근 토큰을 받으세요.",
      "아래 설정을 config.toml에 추가하세요.",
    ],
    configExample: `[channels.matrix]\nhomeserver_url = "https://matrix.org"\nbot_token = "YOUR_BOT_TOKEN"\nallowed_users = ["@you:matrix.org"]`,
  },

  bluebubbles: {
    title: "BlueBubbles Setup",
    titleKo: "BlueBubbles 설정 안내",
    steps: [
      "Install BlueBubbles Server on your Mac.",
      "Note the server URL and password.",
      "Add config below to config.toml.",
    ],
    stepsKo: [
      "Mac에 BlueBubbles Server를 설치하세요.",
      "서버 URL과 비밀번호를 확인하세요.",
      "아래 설정을 config.toml에 추가하세요.",
    ],
    configExample: `[channels.bluebubbles]\nserver_url = "http://192.168.1.100:1234"\npassword = "YOUR_PASSWORD"`,
  },

  clawdtalk: {
    title: "ClawdTalk (Voice) Setup",
    titleKo: "ClawdTalk (음성) 설정 안내",
    steps: [
      "Sign up at telnyx.com and get an API key.",
      "Create a SIP connection and get the Connection ID.",
      "Add config below to config.toml.",
    ],
    stepsKo: [
      "telnyx.com 에 가입하고 API 키를 받으세요.",
      "SIP 연결을 만들고 Connection ID를 복사하세요.",
      "아래 설정을 config.toml에 추가하세요.",
    ],
    configExample: `[channels.clawdtalk]\napi_key = "YOUR_TELNYX_API_KEY"\nconnection_id = "YOUR_SIP_CONNECTION_ID"`,
  },
};

export function ChannelGuide({ channelName, locale, onClose }: ChannelGuideProps) {
  const [copied, setCopied] = useState(false);
  const [inputValues, setInputValues] = useState<Record<string, string>>({});
  const [saving, setSaving] = useState(false);
  const [saveResult, setSaveResult] = useState<string | null>(null);

  const guide = CHANNEL_GUIDES[channelName];

  if (!guide) {
    return (
      <div className="channel-guide-overlay" onClick={onClose}>
        <div className="channel-guide-modal" onClick={(e) => e.stopPropagation()}>
          <div className="channel-guide-header">
            <span>{locale === "ko" ? "안내 없음" : "No guide available"}</span>
            <button className="channel-guide-close" onClick={onClose}>&times;</button>
          </div>
          <div className="channel-guide-body">
            <p>
              {locale === "ko"
                ? `${channelName} 채널에 대한 설정 안내가 아직 준비되지 않았습니다.`
                : `Setup guide for ${channelName} is not yet available.`}
            </p>
          </div>
        </div>
      </div>
    );
  }

  const title = locale === "ko" ? guide.titleKo : guide.title;
  const steps = locale === "ko" ? guide.stepsKo : guide.steps;
  const hasInputFields = guide.inputFields && guide.inputFields.length > 0;

  const handleCopy = () => {
    navigator.clipboard.writeText(guide.configExample).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    });
  };

  const handleInputChange = (key: string, value: string) => {
    setInputValues((prev) => ({ ...prev, [key]: value }));
    setSaveResult(null);
  };

  const handleSave = async () => {
    if (!guide.inputFields) return;

    // Check required fields
    for (const field of guide.inputFields) {
      if (field.required && !inputValues[field.key]?.trim()) {
        const label = locale === "ko" ? field.labelKo : field.label;
        setSaveResult(
          locale === "ko"
            ? `${label}을(를) 입력해 주세요.`
            : `Please enter ${label}.`,
        );
        return;
      }
    }

    // Build config values
    const configValues: Record<string, string> = {};
    for (const field of guide.inputFields) {
      const val = inputValues[field.key]?.trim();
      if (!val) continue;
      if (field.isArray) {
        // Convert comma-separated or single value to JSON array
        const items = val.split(",").map((s) => s.trim()).filter(Boolean);
        configValues[field.key] = JSON.stringify(items);
      } else {
        configValues[field.key] = val;
      }
    }

    setSaving(true);
    setSaveResult(null);

    try {
      const result = await saveChannelConfig(channelName, configValues);
      setSaveResult(
        result ??
          (locale === "ko"
            ? "설정이 저장되었습니다. MoA를 재시작해 주세요."
            : "Configuration saved. Please restart MoA."),
      );
    } catch (e) {
      setSaveResult(
        locale === "ko"
          ? `저장 중 문제가 발생했습니다: ${e}`
          : `Failed to save: ${e}`,
      );
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="channel-guide-overlay" onClick={onClose}>
      <div className="channel-guide-modal" onClick={(e) => e.stopPropagation()}>
        <div className="channel-guide-header">
          <span>{title}</span>
          <button className="channel-guide-close" onClick={onClose}>&times;</button>
        </div>
        <div className="channel-guide-body">
          <ol className="channel-guide-steps">
            {steps.map((step, i) => (
              <li key={i}>{step}</li>
            ))}
          </ol>

          {/* ── Input form (simple, no TOML editing) ── */}
          {hasInputFields && (
            <div className="channel-guide-form">
              {guide.inputFields!.map((field) => (
                <div key={field.key} className="channel-guide-field">
                  <label className="channel-guide-label">
                    {locale === "ko" ? field.labelKo : field.label}
                    {field.required && <span className="channel-guide-required"> *</span>}
                  </label>
                  <input
                    type="text"
                    className="channel-guide-input"
                    placeholder={field.placeholder}
                    value={inputValues[field.key] ?? ""}
                    onChange={(e) => handleInputChange(field.key, e.target.value)}
                    autoComplete="off"
                    spellCheck={false}
                  />
                </div>
              ))}
              <button
                className="channel-guide-save-btn"
                onClick={handleSave}
                disabled={saving}
              >
                {saving
                  ? (locale === "ko" ? "저장 중..." : "Saving...")
                  : (locale === "ko" ? "저장하고 연결하기" : "Save & Connect")}
              </button>
              {saveResult && (
                <p className="channel-guide-save-result">{saveResult}</p>
              )}
            </div>
          )}

          {/* ── Fallback: show config.toml example for channels without input fields ── */}
          {!hasInputFields && (
            <div className="channel-guide-config-section">
              <div className="channel-guide-config-header">
                <span className="channel-guide-config-title">config.toml</span>
                <button className="channel-guide-copy-btn" onClick={handleCopy}>
                  {copied
                    ? (locale === "ko" ? "복사됨!" : "Copied!")
                    : (locale === "ko" ? "복사" : "Copy")}
                </button>
              </div>
              <pre className="channel-guide-config-code">{guide.configExample}</pre>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
