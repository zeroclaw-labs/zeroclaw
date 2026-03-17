import { useState } from "react";
import type { Locale } from "../lib/i18n";

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
}

const CHANNEL_GUIDES: Record<string, GuideContent> = {
  telegram: {
    title: "Telegram Bot Setup",
    titleKo: "텔레그램 봇 설정 안내",
    steps: [
      "Open Telegram and search for @BotFather.",
      "Send /newbot and follow the prompts to create a bot. Choose a name and username.",
      "BotFather will give you a Bot Token (e.g. 123456:ABC-DEF...). Copy it.",
      "To find your User ID: search for @userinfobot on Telegram and send /start. Copy your numeric ID.",
      "Add the config below to your config.toml file, replacing the placeholder values.",
      "Restart MoA. Your bot is now ready! Send a message to your bot on Telegram.",
    ],
    stepsKo: [
      "텔레그램을 열고 @BotFather를 검색하세요.",
      "/newbot 명령어를 보내고 안내에 따라 봇을 만드세요. 이름과 사용자명을 정해주세요.",
      "BotFather가 Bot Token을 알려줍니다 (예: 123456:ABC-DEF...). 이 토큰을 복사하세요.",
      "내 User ID 확인: 텔레그램에서 @userinfobot을 검색하고 /start를 보내면 숫자 ID를 알 수 있습니다.",
      "아래 설정을 config.toml 파일에 추가하고, 값을 실제 정보로 바꿔주세요.",
      "MoA를 재시작하세요. 이제 텔레그램에서 봇에게 메시지를 보내면 작동합니다!",
    ],
    configExample: `[channels.telegram]
bot_token = "YOUR_BOT_TOKEN"
allowed_users = ["YOUR_USER_ID"]`,
  },

  discord: {
    title: "Discord Bot Setup",
    titleKo: "디스코드 봇 설정 안내",
    steps: [
      "Go to https://discord.com/developers/applications and click 'New Application'.",
      "Enter a name for your app and click 'Create'.",
      "Go to 'Bot' in the left sidebar. Click 'Reset Token' and copy the Bot Token.",
      "Under 'Privileged Gateway Intents', enable 'Message Content Intent'.",
      "Go to 'OAuth2' > 'URL Generator'. Select 'bot' scope and 'Send Messages', 'Read Message History' permissions.",
      "Copy the generated URL, open it in a browser, and add the bot to your server.",
      "To find your User ID: enable Developer Mode in Discord settings (App Settings > Advanced), then right-click your username and 'Copy User ID'.",
      "Add the config below to your config.toml file.",
      "Restart MoA. Your Discord bot is now online!",
    ],
    stepsKo: [
      "https://discord.com/developers/applications 에 접속하고 'New Application'을 클릭하세요.",
      "앱 이름을 입력하고 'Create'를 클릭하세요.",
      "왼쪽 메뉴에서 'Bot'으로 이동하세요. 'Reset Token'을 클릭하고 Bot Token을 복사하세요.",
      "'Privileged Gateway Intents'에서 'Message Content Intent'를 활성화하세요.",
      "'OAuth2' > 'URL Generator'에서 'bot' 스코프를 선택하고 'Send Messages', 'Read Message History' 권한을 체크하세요.",
      "생성된 URL을 복사해서 브라우저에서 열고, 봇을 서버에 추가하세요.",
      "내 User ID 확인: 디스코드 설정 > 고급 > 개발자 모드를 켜고, 내 이름을 우클릭해서 'Copy User ID'를 선택하세요.",
      "아래 설정을 config.toml 파일에 추가하세요.",
      "MoA를 재시작하면 디스코드 봇이 온라인됩니다!",
    ],
    configExample: `[channels.discord]
bot_token = "YOUR_BOT_TOKEN"
allowed_users = ["YOUR_USER_ID"]`,
  },

  slack: {
    title: "Slack Bot Setup",
    titleKo: "슬랙 봇 설정 안내",
    steps: [
      "Go to https://api.slack.com/apps and click 'Create New App' > 'From scratch'.",
      "Enter an app name and select your workspace, then click 'Create App'.",
      "Go to 'OAuth & Permissions' and add these Bot Token Scopes: chat:write, channels:history, groups:history, im:history, mpim:history.",
      "Click 'Install to Workspace' at the top and authorize.",
      "Copy the 'Bot User OAuth Token' (starts with xoxb-).",
      "For Socket Mode: go to 'Socket Mode', enable it. Create an App-Level Token with 'connections:write' scope. Copy the token (starts with xapp-).",
      "Go to 'Event Subscriptions', enable events, and subscribe to: message.channels, message.groups, message.im, message.mpim.",
      "To find your User ID: click your profile picture in Slack, select 'Profile', click the '...' menu, and 'Copy member ID'.",
      "Add the config below to your config.toml file.",
      "Restart MoA. Invite your bot to a channel with /invite @botname.",
    ],
    stepsKo: [
      "https://api.slack.com/apps 에 접속하고 'Create New App' > 'From scratch'를 선택하세요.",
      "앱 이름을 입력하고 워크스페이스를 선택한 뒤 'Create App'을 클릭하세요.",
      "'OAuth & Permissions'에서 Bot Token Scopes를 추가하세요: chat:write, channels:history, groups:history, im:history, mpim:history.",
      "'Install to Workspace'를 클릭하고 권한을 승인하세요.",
      "'Bot User OAuth Token'을 복사하세요 (xoxb-로 시작합니다).",
      "소켓 모드: 'Socket Mode'에서 활성화하고 App-Level Token을 만들어 'connections:write' 스코프를 추가하세요. 토큰(xapp-으로 시작)을 복사하세요.",
      "'Event Subscriptions'에서 이벤트를 활성화하고 구독: message.channels, message.groups, message.im, message.mpim.",
      "내 User ID 확인: 슬랙에서 프로필 사진 클릭 > 'Profile' > '...' 메뉴 > 'Copy member ID'를 선택하세요.",
      "아래 설정을 config.toml 파일에 추가하세요.",
      "MoA를 재시작하고 채널에서 /invite @봇이름 으로 봇을 초대하세요.",
    ],
    configExample: `[channels.slack]
bot_token = "xoxb-YOUR-BOT-TOKEN"
app_token = "xapp-YOUR-APP-TOKEN"
allowed_users = ["YOUR_USER_ID"]`,
  },

  mattermost: {
    title: "Mattermost Bot Setup",
    titleKo: "Mattermost 봇 설정 안내",
    steps: [
      "Go to your Mattermost server's System Console > Integrations > Bot Accounts.",
      "Enable 'Enable Bot Account Creation'.",
      "Go to Main Menu > Integrations > Bot Accounts > Add Bot Account.",
      "Set a username and display name. Copy the generated Access Token.",
      "Find your User ID from your Mattermost profile or the API.",
      "Add the config below to your config.toml file.",
      "Restart MoA. Invite the bot to a channel.",
    ],
    stepsKo: [
      "Mattermost 서버의 시스템 콘솔 > 통합 > 봇 계정으로 이동하세요.",
      "'봇 계정 생성 활성화'를 켜주세요.",
      "메인 메뉴 > 통합 > 봇 계정 > 봇 계정 추가를 클릭하세요.",
      "사용자명과 표시 이름을 설정하고 생성된 액세스 토큰을 복사하세요.",
      "프로필이나 API에서 내 User ID를 확인하세요.",
      "아래 설정을 config.toml 파일에 추가하세요.",
      "MoA를 재시작하고 봇을 채널에 초대하세요.",
    ],
    configExample: `[channels.mattermost]
url = "https://your-mattermost-server.com"
bot_token = "YOUR_BOT_TOKEN"
allowed_users = ["YOUR_USER_ID"]`,
  },

  whatsapp: {
    title: "WhatsApp Setup",
    titleKo: "WhatsApp 설정 안내",
    steps: [
      "Go to https://developers.facebook.com and create a new app (type: Business).",
      "Add the 'WhatsApp' product to your app.",
      "In WhatsApp > API Setup, you'll find a temporary Access Token and Phone Number ID. Copy both.",
      "Set up a webhook: in WhatsApp > Configuration, set the callback URL to your server (e.g. https://your-domain/webhook/whatsapp).",
      "Set a Verify Token (any string you choose). Subscribe to 'messages' webhook field.",
      "Copy the App Secret from Settings > Basic for webhook signature verification.",
      "Add the config below to your config.toml file.",
      "Restart MoA. Send a message from your phone to the WhatsApp test number to verify.",
    ],
    stepsKo: [
      "https://developers.facebook.com 에 접속하고 새 앱을 만드세요 (유형: 비즈니스).",
      "앱에 'WhatsApp' 제품을 추가하세요.",
      "WhatsApp > API Setup에서 임시 Access Token과 Phone Number ID를 복사하세요.",
      "웹훅 설정: WhatsApp > Configuration에서 콜백 URL을 입력하세요 (예: https://your-domain/webhook/whatsapp).",
      "Verify Token을 설정하세요 (원하는 문자열). 'messages' 웹훅 필드를 구독하세요.",
      "설정 > 기본에서 App Secret을 복사하세요 (웹훅 서명 검증용).",
      "아래 설정을 config.toml 파일에 추가하세요.",
      "MoA를 재시작하고 WhatsApp 테스트 번호로 메시지를 보내서 확인하세요.",
    ],
    configExample: `[channels.whatsapp]
access_token = "YOUR_ACCESS_TOKEN"
phone_number_id = "YOUR_PHONE_NUMBER_ID"
verify_token = "YOUR_VERIFY_TOKEN"
app_secret = "YOUR_APP_SECRET"
allowed_numbers = ["+821012345678"]`,
  },

  line: {
    title: "LINE Bot Setup",
    titleKo: "LINE 봇 설정 안내",
    steps: [
      "Go to https://developers.line.biz and log in.",
      "Create a new Provider (or use an existing one), then create a new 'Messaging API' channel.",
      "In the channel settings, find the 'Channel Secret' and 'Channel Access Token' (issue one if needed).",
      "Set the Webhook URL to your server (e.g. https://your-domain/webhook/line) and enable 'Use webhook'.",
      "Disable 'Auto-reply messages' and 'Greeting messages' in LINE Official Account settings.",
      "Find your LINE user ID in the channel's 'Basic settings' page (Your user ID).",
      "Add the config below to your config.toml file.",
      "Restart MoA. Send a message to your LINE bot to test.",
    ],
    stepsKo: [
      "https://developers.line.biz 에 접속해서 로그인하세요.",
      "새 Provider를 만들고 (또는 기존 것 사용) 'Messaging API' 채널을 만드세요.",
      "채널 설정에서 'Channel Secret'과 'Channel Access Token'을 확인하세요 (없으면 발급하세요).",
      "Webhook URL을 서버 주소로 설정하세요 (예: https://your-domain/webhook/line). 'Use webhook'을 활성화하세요.",
      "LINE 공식 계정 설정에서 '자동 응답 메시지'와 '인사 메시지'를 비활성화하세요.",
      "'Basic settings' 페이지에서 내 LINE User ID를 확인하세요.",
      "아래 설정을 config.toml 파일에 추가하세요.",
      "MoA를 재시작하고 LINE 봇에 메시지를 보내서 테스트하세요.",
    ],
    configExample: `[channels.line]
channel_secret = "YOUR_CHANNEL_SECRET"
channel_access_token = "YOUR_CHANNEL_ACCESS_TOKEN"
allowed_users = ["YOUR_LINE_USER_ID"]`,
  },

  kakao: {
    title: "KakaoTalk Bot Setup",
    titleKo: "카카오톡 봇 설정 안내",
    steps: [
      "Go to https://developers.kakao.com and create a new application.",
      "In the app settings, enable 'Kakao Talk Channel' and link your channel.",
      "Go to Platform > Web and register your domain.",
      "In 'Kakao Login', enable it and add required scopes.",
      "Find your REST API Key and Admin Key in the app's 'Keys' section.",
      "Set up a webhook/callback URL for message reception.",
      "Add the config below to your config.toml file.",
      "Restart MoA. Send a message to your KakaoTalk channel to test.",
    ],
    stepsKo: [
      "https://developers.kakao.com 에 접속해서 새 애플리케이션을 만드세요.",
      "앱 설정에서 '카카오톡 채널'을 활성화하고 채널을 연결하세요.",
      "플랫폼 > Web에서 도메인을 등록하세요.",
      "'카카오 로그인'을 활성화하고 필요한 스코프를 추가하세요.",
      "앱의 '키' 섹션에서 REST API Key와 Admin Key를 확인하세요.",
      "메시지 수신을 위한 웹훅/콜백 URL을 설정하세요.",
      "아래 설정을 config.toml 파일에 추가하세요.",
      "MoA를 재시작하고 카카오톡 채널에 메시지를 보내서 테스트하세요.",
    ],
    configExample: `[channels.kakao]
rest_api_key = "YOUR_REST_API_KEY"
admin_key = "YOUR_ADMIN_KEY"
allowed_users = ["YOUR_KAKAO_USER_ID"]`,
  },

  matrix: {
    title: "Matrix Bot Setup",
    titleKo: "Matrix 봇 설정 안내",
    steps: [
      "Create a new Matrix account for your bot on your homeserver (e.g. matrix.org).",
      "Log in to the bot account and get an access token (from Element: Settings > Help & About > Access Token).",
      "Create a room or use an existing one. Copy the Room ID (in Element: Room Settings > Advanced > Internal Room ID).",
      "Invite the bot user to the room.",
      "Find your User ID (e.g. @yourname:matrix.org).",
      "Add the config below to your config.toml file.",
      "Restart MoA. Send a message in the room to test.",
    ],
    stepsKo: [
      "홈서버(예: matrix.org)에서 봇용 새 Matrix 계정을 만드세요.",
      "봇 계정으로 로그인하고 액세스 토큰을 얻으세요 (Element 앱: 설정 > 도움말 및 정보 > Access Token).",
      "새 방을 만들거나 기존 방의 Room ID를 복사하세요 (Element: 방 설정 > 고급 > Internal Room ID).",
      "봇 사용자를 방에 초대하세요.",
      "내 User ID를 확인하세요 (예: @이름:matrix.org).",
      "아래 설정을 config.toml 파일에 추가하세요.",
      "MoA를 재시작하고 방에서 메시지를 보내서 테스트하세요.",
    ],
    configExample: `[channels.matrix]
homeserver = "https://matrix.org"
access_token = "YOUR_ACCESS_TOKEN"
room_id = "!your_room_id:matrix.org"
allowed_users = ["@your_user:matrix.org"]`,
  },

  signal: {
    title: "Signal Bot Setup",
    titleKo: "Signal 봇 설정 안내",
    steps: [
      "Install signal-cli (https://github.com/AsamK/signal-cli) and register a phone number.",
      "Start signal-cli in HTTP daemon mode: signal-cli -a +YOUR_NUMBER daemon --http=8686",
      "Find your phone number in E.164 format (e.g. +1234567890).",
      "Add the config below to your config.toml file.",
      "Restart MoA. Send a message to the Signal number to test.",
    ],
    stepsKo: [
      "signal-cli를 설치하세요 (https://github.com/AsamK/signal-cli) 그리고 전화번호를 등록하세요.",
      "signal-cli를 HTTP 데몬 모드로 시작하세요: signal-cli -a +전화번호 daemon --http=8686",
      "전화번호를 E.164 형식으로 확인하세요 (예: +821012345678).",
      "아래 설정을 config.toml 파일에 추가하세요.",
      "MoA를 재시작하고 해당 Signal 번호로 메시지를 보내서 테스트하세요.",
    ],
    configExample: `[channels.signal]
http_url = "http://127.0.0.1:8686"
account = "+YOUR_PHONE_NUMBER"
allowed_senders = ["+ALLOWED_PHONE_NUMBER"]`,
  },

  irc: {
    title: "IRC Bot Setup",
    titleKo: "IRC 봇 설정 안내",
    steps: [
      "Choose an IRC server (e.g. irc.libera.chat) and a channel to join.",
      "Decide on a nickname for your bot.",
      "(Optional) Register the nickname with NickServ if the server supports it.",
      "Add the config below to your config.toml file.",
      "Restart MoA. The bot will join the specified channel automatically.",
    ],
    stepsKo: [
      "IRC 서버를 선택하세요 (예: irc.libera.chat) 그리고 참여할 채널을 정하세요.",
      "봇의 닉네임을 정하세요.",
      "(선택) 서버가 지원하면 NickServ에 닉네임을 등록하세요.",
      "아래 설정을 config.toml 파일에 추가하세요.",
      "MoA를 재시작하면 봇이 지정된 채널에 자동으로 접속합니다.",
    ],
    configExample: `[channels.irc]
server = "irc.libera.chat"
port = 6697
use_tls = true
nickname = "moa-bot"
channel = "#your-channel"`,
  },

  lark: {
    title: "Lark Bot Setup",
    titleKo: "Lark 봇 설정 안내",
    steps: [
      "Go to https://open.larksuite.com and create a new app.",
      "In the app dashboard, find 'App ID' and 'App Secret' under Credentials.",
      "Go to 'Event Subscriptions', set the Request URL to your server (e.g. https://your-domain/webhook/lark).",
      "Add the 'im.message.receive_v1' event.",
      "Go to 'Permissions & Scopes' and add: im:message, im:message:send, im:chat.",
      "Publish the app version and install to your organization.",
      "Add the config below to your config.toml file.",
      "Restart MoA. Send a message to the bot in Lark to test.",
    ],
    stepsKo: [
      "https://open.larksuite.com 에 접속하고 새 앱을 만드세요.",
      "앱 대시보드에서 Credentials 아래의 'App ID'와 'App Secret'을 확인하세요.",
      "'Event Subscriptions'에서 Request URL을 서버 주소로 설정하세요 (예: https://your-domain/webhook/lark).",
      "'im.message.receive_v1' 이벤트를 추가하세요.",
      "'Permissions & Scopes'에서 추가: im:message, im:message:send, im:chat.",
      "앱 버전을 배포하고 조직에 설치하세요.",
      "아래 설정을 config.toml 파일에 추가하세요.",
      "MoA를 재시작하고 Lark에서 봇에 메시지를 보내서 테스트하세요.",
    ],
    configExample: `[channels.lark]
app_id = "YOUR_APP_ID"
app_secret = "YOUR_APP_SECRET"`,
  },

  feishu: {
    title: "Feishu Bot Setup",
    titleKo: "Feishu(비서) 봇 설정 안내",
    steps: [
      "Go to https://open.feishu.cn and create a new app.",
      "In the app dashboard, find 'App ID' and 'App Secret' under Credentials.",
      "Go to 'Event Subscriptions', set the Request URL to your server (e.g. https://your-domain/webhook/feishu).",
      "Add the 'im.message.receive_v1' event.",
      "Go to 'Permissions & Scopes' and add: im:message, im:message:send, im:chat.",
      "Publish the app version and install to your organization.",
      "Add the config below to your config.toml file.",
      "Restart MoA. Send a message to the bot in Feishu to test.",
    ],
    stepsKo: [
      "https://open.feishu.cn 에 접속하고 새 앱을 만드세요.",
      "앱 대시보드에서 Credentials 아래의 'App ID'와 'App Secret'을 확인하세요.",
      "'Event Subscriptions'에서 Request URL을 서버 주소로 설정하세요 (예: https://your-domain/webhook/feishu).",
      "'im.message.receive_v1' 이벤트를 추가하세요.",
      "'Permissions & Scopes'에서 추가: im:message, im:message:send, im:chat.",
      "앱 버전을 배포하고 조직에 설치하세요.",
      "아래 설정을 config.toml 파일에 추가하세요.",
      "MoA를 재시작하고 Feishu에서 봇에 메시지를 보내서 테스트하세요.",
    ],
    configExample: `[channels.feishu]
app_id = "YOUR_APP_ID"
app_secret = "YOUR_APP_SECRET"`,
  },

  dingtalk: {
    title: "DingTalk Bot Setup",
    titleKo: "DingTalk(딩톡) 봇 설정 안내",
    steps: [
      "Go to https://open-dev.dingtalk.com and create a new app.",
      "Find the 'AppKey' and 'AppSecret' in the app credentials.",
      "Create a robot under the app and set the message receiving URL.",
      "Add the config below to your config.toml file.",
      "Restart MoA. Send a message to the DingTalk bot to test.",
    ],
    stepsKo: [
      "https://open-dev.dingtalk.com 에 접속하고 새 앱을 만드세요.",
      "앱 자격증명에서 'AppKey'와 'AppSecret'을 확인하세요.",
      "앱 아래에서 로봇을 만들고 메시지 수신 URL을 설정하세요.",
      "아래 설정을 config.toml 파일에 추가하세요.",
      "MoA를 재시작하고 DingTalk 봇에 메시지를 보내서 테스트하세요.",
    ],
    configExample: `[channels.dingtalk]
client_id = "YOUR_APP_KEY"
client_secret = "YOUR_APP_SECRET"`,
  },

  nostr: {
    title: "Nostr Bot Setup",
    titleKo: "Nostr 봇 설정 안내",
    steps: [
      "Generate a Nostr private key (nsec) for your bot, or use an existing one.",
      "Choose relay URLs to connect to (e.g. wss://relay.damus.io).",
      "Add the config below to your config.toml file.",
      "Restart MoA. Send a DM to the bot's public key on Nostr.",
    ],
    stepsKo: [
      "봇용 Nostr 비밀키(nsec)를 생성하거나 기존 것을 사용하세요.",
      "연결할 릴레이 URL을 선택하세요 (예: wss://relay.damus.io).",
      "아래 설정을 config.toml 파일에 추가하세요.",
      "MoA를 재시작하고 Nostr에서 봇의 공개키로 DM을 보내세요.",
    ],
    configExample: `[channels.nostr]
private_key = "nsec1YOUR_PRIVATE_KEY"
relays = ["wss://relay.damus.io"]`,
  },

  github: {
    title: "GitHub Channel Setup",
    titleKo: "GitHub 채널 설정 안내",
    steps: [
      "Go to https://github.com/settings/tokens and create a Fine-grained Personal Access Token.",
      "Select the repositories and grant 'Issues: Read and Write' and 'Pull Requests: Read and Write' permissions.",
      "Copy the generated token.",
      "(Optional) Set up a webhook in your repo: Settings > Webhooks > Add webhook. Set the Payload URL to your server and Content type to application/json.",
      "Add the config below to your config.toml file.",
      "Restart MoA. The bot can now respond to GitHub issues and PRs.",
    ],
    stepsKo: [
      "https://github.com/settings/tokens 에서 Fine-grained Personal Access Token을 만드세요.",
      "저장소를 선택하고 'Issues: Read and Write', 'Pull Requests: Read and Write' 권한을 부여하세요.",
      "생성된 토큰을 복사하세요.",
      "(선택) 저장소에 웹훅 설정: Settings > Webhooks > Add webhook. Payload URL을 서버 주소로 설정하고 Content type을 application/json으로 하세요.",
      "아래 설정을 config.toml 파일에 추가하세요.",
      "MoA를 재시작하면 봇이 GitHub 이슈와 PR에 응답할 수 있습니다.",
    ],
    configExample: `[channels.github]
access_token = "github_pat_YOUR_TOKEN"
# webhook_secret = "your_webhook_secret"  # optional`,
  },

  email: {
    title: "Email Channel Setup",
    titleKo: "이메일 채널 설정 안내",
    steps: [
      "Prepare your email IMAP/SMTP server credentials.",
      "For Gmail: enable 'App passwords' in your Google Account security settings and generate one.",
      "For other providers: get your IMAP server address, SMTP server address, and credentials.",
      "Add the config below to your config.toml file.",
      "Restart MoA. Send an email to the configured address to test.",
    ],
    stepsKo: [
      "이메일 IMAP/SMTP 서버 자격증명을 준비하세요.",
      "Gmail인 경우: Google 계정 보안 설정에서 '앱 비밀번호'를 활성화하고 하나를 생성하세요.",
      "다른 제공자인 경우: IMAP 서버 주소, SMTP 서버 주소, 자격증명을 확인하세요.",
      "아래 설정을 config.toml 파일에 추가하세요.",
      "MoA를 재시작하고 설정된 이메일 주소로 메시지를 보내서 테스트하세요.",
    ],
    configExample: `[channels.email]
imap_host = "imap.gmail.com"
imap_port = 993
smtp_host = "smtp.gmail.com"
smtp_port = 465
username = "your_email@gmail.com"
password = "your_app_password"
from_address = "your_email@gmail.com"
allowed_senders = ["friend@example.com"]`,
  },

  webhook: {
    title: "Webhook Channel Setup",
    titleKo: "웹훅 채널 설정 안내",
    steps: [
      "Decide on a port for receiving incoming webhooks.",
      "(Optional) Set a shared secret for webhook signature verification.",
      "Add the config below to your config.toml file.",
      "Restart MoA. Send a POST request to http://your-server:PORT/webhook to test.",
    ],
    stepsKo: [
      "들어오는 웹훅을 수신할 포트를 정하세요.",
      "(선택) 웹훅 서명 검증을 위한 공유 시크릿을 설정하세요.",
      "아래 설정을 config.toml 파일에 추가하세요.",
      "MoA를 재시작하고 http://서버주소:포트/webhook 으로 POST 요청을 보내서 테스트하세요.",
    ],
    configExample: `[channels.webhook]
port = 9090
# secret = "your_shared_secret"  # optional`,
  },

  imessage: {
    title: "iMessage Setup",
    titleKo: "iMessage 설정 안내",
    steps: [
      "This channel works only on macOS with iMessage configured.",
      "Make sure iMessage is signed in on your Mac.",
      "Find the phone numbers or email addresses you want to allow.",
      "Add the config below to your config.toml file.",
      "Restart MoA. The bot will respond to allowed iMessage contacts.",
    ],
    stepsKo: [
      "이 채널은 iMessage가 설정된 macOS에서만 작동합니다.",
      "Mac에서 iMessage에 로그인되어 있는지 확인하세요.",
      "허용할 전화번호나 이메일 주소를 확인하세요.",
      "아래 설정을 config.toml 파일에 추가하세요.",
      "MoA를 재시작하면 허용된 iMessage 연락처에 응답합니다.",
    ],
    configExample: `[channels.imessage]
allowed_contacts = ["+1234567890", "friend@icloud.com"]`,
  },

  qq: {
    title: "QQ Official Bot Setup",
    titleKo: "QQ 공식 봇 설정 안내",
    steps: [
      "Go to https://q.qq.com and register as a QQ Bot developer.",
      "Create a new bot application and get the App ID and Token.",
      "Set the callback URL for message events.",
      "Add the config below to your config.toml file.",
      "Restart MoA. Send a message to the QQ bot to test.",
    ],
    stepsKo: [
      "https://q.qq.com 에 접속하고 QQ 봇 개발자로 등록하세요.",
      "새 봇 애플리케이션을 만들고 App ID와 Token을 받으세요.",
      "메시지 이벤트용 콜백 URL을 설정하세요.",
      "아래 설정을 config.toml 파일에 추가하세요.",
      "MoA를 재시작하고 QQ 봇에 메시지를 보내서 테스트하세요.",
    ],
    configExample: `[channels.qq]
app_id = "YOUR_APP_ID"
app_secret = "YOUR_APP_SECRET"`,
  },

  napcat: {
    title: "NapCat (QQ) Setup",
    titleKo: "NapCat (QQ) 설정 안내",
    steps: [
      "Install NapCat (OneBot v11 compatible QQ protocol): https://github.com/NapNeko/NapCatQQ",
      "Configure NapCat to run with your QQ account.",
      "Set the HTTP or WebSocket endpoint URL.",
      "Add the config below to your config.toml file.",
      "Restart MoA. Send a QQ message to test.",
    ],
    stepsKo: [
      "NapCat을 설치하세요 (OneBot v11 호환 QQ 프로토콜): https://github.com/NapNeko/NapCatQQ",
      "NapCat을 QQ 계정으로 실행하도록 설정하세요.",
      "HTTP 또는 WebSocket 엔드포인트 URL을 설정하세요.",
      "아래 설정을 config.toml 파일에 추가하세요.",
      "MoA를 재시작하고 QQ 메시지를 보내서 테스트하세요.",
    ],
    configExample: `[channels.napcat]
websocket_url = "ws://127.0.0.1:3001"
# access_token = "your_token"  # optional`,
  },

  bluebubbles: {
    title: "BlueBubbles Setup",
    titleKo: "BlueBubbles 설정 안내",
    steps: [
      "Install BlueBubbles Server on your Mac (https://bluebubbles.app).",
      "Start the server and set a server password.",
      "Note the server URL (e.g. http://192.168.1.100:1234 or your ngrok URL).",
      "Add the config below to your config.toml file.",
      "Restart MoA. Send an iMessage through BlueBubbles to test.",
    ],
    stepsKo: [
      "Mac에 BlueBubbles Server를 설치하세요 (https://bluebubbles.app).",
      "서버를 시작하고 서버 비밀번호를 설정하세요.",
      "서버 URL을 확인하세요 (예: http://192.168.1.100:1234 또는 ngrok URL).",
      "아래 설정을 config.toml 파일에 추가하세요.",
      "MoA를 재시작하고 BlueBubbles를 통해 iMessage를 보내서 테스트하세요.",
    ],
    configExample: `[channels.bluebubbles]
server_url = "http://192.168.1.100:1234"
password = "YOUR_SERVER_PASSWORD"
allowed_senders = ["+1234567890"]`,
  },

  linq: {
    title: "Linq Setup",
    titleKo: "Linq 설정 안내",
    steps: [
      "Sign up for a Linq Partner API account and get your API token.",
      "Register a phone number for sending messages (E.164 format).",
      "Add the config below to your config.toml file.",
      "Restart MoA. Send a message to the Linq number to test.",
    ],
    stepsKo: [
      "Linq Partner API 계정을 만들고 API 토큰을 받으세요.",
      "메시지 전송용 전화번호를 등록하세요 (E.164 형식).",
      "아래 설정을 config.toml 파일에 추가하세요.",
      "MoA를 재시작하고 Linq 번호로 메시지를 보내서 테스트하세요.",
    ],
    configExample: `[channels.linq]
api_token = "YOUR_API_TOKEN"
from_phone = "+1234567890"
allowed_senders = ["+9876543210"]`,
  },

  wati: {
    title: "WATI Setup",
    titleKo: "WATI 설정 안내",
    steps: [
      "Sign up at https://www.wati.io and get your API token.",
      "Your WATI dashboard will show the API URL and token.",
      "Add the config below to your config.toml file.",
      "Restart MoA. Send a WhatsApp message through WATI to test.",
    ],
    stepsKo: [
      "https://www.wati.io 에 가입하고 API 토큰을 받으세요.",
      "WATI 대시보드에서 API URL과 토큰을 확인하세요.",
      "아래 설정을 config.toml 파일에 추가하세요.",
      "MoA를 재시작하고 WATI를 통해 WhatsApp 메시지를 보내서 테스트하세요.",
    ],
    configExample: `[channels.wati]
api_token = "YOUR_API_TOKEN"
# api_url = "https://live-mt-server.wati.io"  # default
allowed_numbers = ["+1234567890"]`,
  },

  nextcloud_talk: {
    title: "Nextcloud Talk Setup",
    titleKo: "Nextcloud Talk 설정 안내",
    steps: [
      "Go to your Nextcloud admin panel and enable the Talk app.",
      "Register a Bot app and get the app token (OCS API).",
      "Set the webhook URL for message events.",
      "Add the config below to your config.toml file.",
      "Restart MoA. Send a message in Nextcloud Talk to test.",
    ],
    stepsKo: [
      "Nextcloud 관리자 패널에서 Talk 앱을 활성화하세요.",
      "Bot 앱을 등록하고 앱 토큰(OCS API)을 받으세요.",
      "메시지 이벤트용 웹훅 URL을 설정하세요.",
      "아래 설정을 config.toml 파일에 추가하세요.",
      "MoA를 재시작하고 Nextcloud Talk에서 메시지를 보내서 테스트하세요.",
    ],
    configExample: `[channels.nextcloud_talk]
base_url = "https://cloud.example.com"
app_token = "YOUR_BOT_APP_TOKEN"
allowed_users = ["your_user_id"]`,
  },

  acp: {
    title: "ACP (Agent Client Protocol) Setup",
    titleKo: "ACP 설정 안내",
    steps: [
      "ACP connects MoA to OpenCode or compatible agent clients.",
      "Ensure OpenCode is installed (or set a custom path).",
      "Add the config below to your config.toml file.",
      "Restart MoA. The ACP channel will connect automatically.",
    ],
    stepsKo: [
      "ACP는 MoA를 OpenCode 또는 호환 에이전트 클라이언트에 연결합니다.",
      "OpenCode가 설치되어 있는지 확인하세요 (또는 경로를 지정하세요).",
      "아래 설정을 config.toml 파일에 추가하세요.",
      "MoA를 재시작하면 ACP 채널이 자동으로 연결됩니다.",
    ],
    configExample: `[channels.acp]
# opencode_path = "opencode"  # default
# workdir = "/path/to/workspace"  # optional`,
  },

  clawdtalk: {
    title: "ClawdTalk (Voice) Setup",
    titleKo: "ClawdTalk (음성) 설정 안내",
    steps: [
      "Sign up at https://telnyx.com and get an API key.",
      "Create a SIP connection in Telnyx and copy the Connection ID.",
      "Register a phone number for making/receiving calls.",
      "Add the config below to your config.toml file.",
      "Restart MoA. Call the number to test voice interaction.",
    ],
    stepsKo: [
      "https://telnyx.com 에 가입하고 API 키를 받으세요.",
      "Telnyx에서 SIP 연결을 만들고 Connection ID를 복사하세요.",
      "전화 발신/수신용 전화번호를 등록하세요.",
      "아래 설정을 config.toml 파일에 추가하세요.",
      "MoA를 재시작하고 전화를 걸어 음성 상호작용을 테스트하세요.",
    ],
    configExample: `[channels.clawdtalk]
api_key = "YOUR_TELNYX_API_KEY"
connection_id = "YOUR_SIP_CONNECTION_ID"
from_number = "+1234567890"`,
  },
};

export function ChannelGuide({ channelName, locale, onClose }: ChannelGuideProps) {
  const [copied, setCopied] = useState(false);
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
                ? `${channelName} 채널에 대한 설정 안내가 아직 준비되지 않았습니다. config.toml 파일을 참조하세요.`
                : `Setup guide for ${channelName} is not yet available. Please refer to config.toml.`}
            </p>
          </div>
        </div>
      </div>
    );
  }

  const title = locale === "ko" ? guide.titleKo : guide.title;
  const steps = locale === "ko" ? guide.stepsKo : guide.steps;

  const handleCopy = () => {
    navigator.clipboard.writeText(guide.configExample).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    });
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
        </div>
      </div>
    </div>
  );
}
