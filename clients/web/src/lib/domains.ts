// Domain configuration for ZeroClaw Agent Selection System
// Each domain has sub-agents, recommended LLMs, and tools (APIs)

export interface LLMModel {
  id: string;
  name: string;
  provider: string;
  description: string;
  descriptionKo: string;
  strengths: string[];
  tier: "free" | "pro" | "enterprise";
}

export interface ToolAPI {
  id: string;
  name: string;
  description: string;
  descriptionKo: string;
  apiType: "rest" | "sdk" | "browser" | "realtime";
  category: string;
}

export interface ChannelOption {
  id: string;
  name: string;
  icon: string;
  description: string;
}

export interface SubAgent {
  id: string;
  name: string;
  nameKo: string;
  description: string;
  descriptionKo: string;
  recommendedLLMs: string[]; // LLM model IDs
  recommendedTools: string[]; // Tool IDs
  alternativeLLMs: string[]; // Additional LLM options
  alternativeTools: string[]; // Additional tool options
}

export interface Domain {
  id: string;
  name: string;
  nameKo: string;
  icon: string;
  color: string;
  subAgents: SubAgent[];
}

// ============================================================
// LLM Models Registry
// ============================================================
export const llmModels: LLMModel[] = [
  // --- Anthropic ---
  {
    id: "claude-opus-4.6",
    name: "Claude Opus 4.6",
    provider: "Anthropic",
    description: "Top-tier reasoning, coding, long-document analysis",
    descriptionKo: "최상위 추론, 코딩, 장문서 분석",
    strengths: ["reasoning", "coding", "long-context", "safety"],
    tier: "pro",
  },
  {
    id: "claude-sonnet-4",
    name: "Claude Sonnet 4",
    provider: "Anthropic",
    description: "Balanced performance and cost for general tasks",
    descriptionKo: "범용 작업에 성능과 비용 균형",
    strengths: ["general", "coding", "writing"],
    tier: "pro",
  },
  {
    id: "claude-haiku-3.5",
    name: "Claude Haiku 3.5",
    provider: "Anthropic",
    description: "Fast, lightweight for simple tasks and classification",
    descriptionKo: "단순 작업 및 분류에 빠르고 경량",
    strengths: ["speed", "classification", "low-cost"],
    tier: "free",
  },
  // --- Google ---
  {
    id: "gemini-2.5-pro",
    name: "Gemini 2.5 Pro",
    provider: "Google",
    description: "Advanced reasoning with thinking mode, strong multilingual",
    descriptionKo: "사고 모드 포함 고급 추론, 강력한 다국어 지원",
    strengths: ["reasoning", "multilingual", "multimodal", "long-context"],
    tier: "pro",
  },
  {
    id: "gemini-2.5-flash",
    name: "Gemini 2.5 Flash",
    provider: "Google",
    description: "Fast multimodal processing, great for real-time tasks",
    descriptionKo: "빠른 멀티모달 처리, 실시간 작업에 적합",
    strengths: ["speed", "multimodal", "realtime"],
    tier: "free",
  },
  {
    id: "gemini-2.5-flash-live",
    name: "Gemini 2.5 Flash Live API",
    provider: "Google",
    description: "Real-time voice/video streaming for live interpretation",
    descriptionKo: "실시간 음성/영상 스트리밍 통역용",
    strengths: ["realtime", "voice", "streaming", "interpretation"],
    tier: "pro",
  },
  // --- OpenAI ---
  {
    id: "gpt-4.1",
    name: "GPT-4.1",
    provider: "OpenAI",
    description: "Strong coding and instruction following, large context",
    descriptionKo: "강력한 코딩 및 지시 따르기, 대용량 컨텍스트",
    strengths: ["coding", "instruction-following", "long-context"],
    tier: "pro",
  },
  {
    id: "gpt-4.1-mini",
    name: "GPT-4.1 Mini",
    provider: "OpenAI",
    description: "Cost-effective for routine tasks with good quality",
    descriptionKo: "양질의 일상 작업에 비용 효율적",
    strengths: ["speed", "cost-effective", "general"],
    tier: "free",
  },
  {
    id: "gpt-4o",
    name: "GPT-4o",
    provider: "OpenAI",
    description: "Multimodal with vision, strong general purpose",
    descriptionKo: "비전 포함 멀티모달, 강력한 범용",
    strengths: ["multimodal", "vision", "general"],
    tier: "pro",
  },
  {
    id: "o3",
    name: "o3",
    provider: "OpenAI",
    description: "Advanced reasoning model for complex problem solving",
    descriptionKo: "복잡한 문제 해결을 위한 고급 추론 모델",
    strengths: ["reasoning", "math", "science", "coding"],
    tier: "enterprise",
  },
  // --- DeepSeek ---
  {
    id: "deepseek-r1",
    name: "DeepSeek R1",
    provider: "DeepSeek",
    description: "Open-source reasoning model, strong at math and coding",
    descriptionKo: "오픈소스 추론 모델, 수학/코딩에 강점",
    strengths: ["reasoning", "math", "coding", "open-source"],
    tier: "free",
  },
  {
    id: "deepseek-v3",
    name: "DeepSeek V3",
    provider: "DeepSeek",
    description: "High-quality general model, cost-effective",
    descriptionKo: "고품질 범용 모델, 비용 효율적",
    strengths: ["general", "cost-effective", "multilingual"],
    tier: "free",
  },
  // --- Local / Ollama ---
  {
    id: "llama-3.3-70b",
    name: "Llama 3.3 70B",
    provider: "Ollama / Local",
    description: "Open-source local model for privacy-first deployments",
    descriptionKo: "프라이버시 우선 배포용 오픈소스 로컬 모델",
    strengths: ["local", "privacy", "general", "open-source"],
    tier: "free",
  },
  {
    id: "qwen-2.5-72b",
    name: "Qwen 2.5 72B",
    provider: "Ollama / Local",
    description: "Strong multilingual support, especially CJK languages",
    descriptionKo: "강력한 다국어 지원, 특히 CJK 언어",
    strengths: ["multilingual", "korean", "chinese", "local"],
    tier: "free",
  },
  // --- Specialized ---
  {
    id: "upstage-solar",
    name: "Upstage Solar",
    provider: "Upstage",
    description: "Korean-focused LLM with strong document understanding",
    descriptionKo: "한국어 특화 LLM, 강력한 문서 이해",
    strengths: ["korean", "document", "ocr"],
    tier: "pro",
  },
];

// ============================================================
// Tools (APIs) Registry
// ============================================================
export const toolAPIs: ToolAPI[] = [
  // --- Browser Automation ---
  {
    id: "playwright",
    name: "Playwright",
    description: "Cross-browser automation (Chrome/Firefox/Safari), login, scraping, form submission",
    descriptionKo: "크로스 브라우저 자동화, 로그인, 크롤링, 폼 제출",
    apiType: "sdk",
    category: "browser",
  },
  {
    id: "puppeteer",
    name: "Puppeteer",
    description: "Chrome/Chromium automation for scraping and testing",
    descriptionKo: "Chrome/Chromium 스크래핑 및 테스트 자동화",
    apiType: "sdk",
    category: "browser",
  },
  {
    id: "browserless",
    name: "Browserless API",
    description: "Headless Chrome as a service, no infra needed",
    descriptionKo: "서비스형 헤드리스 크롬, 인프라 불필요",
    apiType: "rest",
    category: "browser",
  },
  // --- Shopping / E-commerce ---
  {
    id: "shopify-api",
    name: "Shopify Admin API",
    description: "Product, order, inventory management for Shopify stores",
    descriptionKo: "Shopify 상품, 주문, 재고 관리",
    apiType: "rest",
    category: "ecommerce",
  },
  {
    id: "cafe24-api",
    name: "Cafe24 API",
    description: "Korean e-commerce platform product/order management",
    descriptionKo: "카페24 쇼핑몰 상품/주문 관리",
    apiType: "rest",
    category: "ecommerce",
  },
  {
    id: "coupang-api",
    name: "Coupang Partners API",
    description: "Coupang seller product listing and inventory sync",
    descriptionKo: "쿠팡 셀러 상품 등록 및 재고 동기화",
    apiType: "rest",
    category: "ecommerce",
  },
  // --- Calendar / Productivity ---
  {
    id: "google-calendar",
    name: "Google Calendar API",
    description: "Calendar events, scheduling, reminders management",
    descriptionKo: "캘린더 이벤트, 일정, 리마인더 관리",
    apiType: "rest",
    category: "productivity",
  },
  {
    id: "google-tasks",
    name: "Google Tasks API",
    description: "Task lists and to-do management",
    descriptionKo: "작업 목록 및 할일 관리",
    apiType: "rest",
    category: "productivity",
  },
  {
    id: "notion-api",
    name: "Notion API",
    description: "Workspace pages, databases, notes management",
    descriptionKo: "워크스페이스 페이지, DB, 메모 관리",
    apiType: "rest",
    category: "productivity",
  },
  {
    id: "todoist-api",
    name: "Todoist API",
    description: "Task management and productivity tracking",
    descriptionKo: "작업 관리 및 생산성 추적",
    apiType: "rest",
    category: "productivity",
  },
  {
    id: "ms-graph",
    name: "Microsoft Graph API",
    description: "Outlook calendar, mail, OneDrive, Teams integration",
    descriptionKo: "Outlook 캘린더, 메일, OneDrive, Teams 통합",
    apiType: "rest",
    category: "productivity",
  },
  // --- Maps / Travel ---
  {
    id: "google-maps",
    name: "Google Maps API",
    description: "Directions, places, geocoding, distance matrix",
    descriptionKo: "길찾기, 장소, 지오코딩, 거리 계산",
    apiType: "rest",
    category: "maps",
  },
  {
    id: "skyscanner-api",
    name: "Skyscanner API",
    description: "Flight search and price comparison",
    descriptionKo: "항공권 검색 및 가격 비교",
    apiType: "rest",
    category: "travel",
  },
  {
    id: "amadeus-api",
    name: "Amadeus API",
    description: "Flight, hotel, car rental booking and search",
    descriptionKo: "항공, 호텔, 렌터카 예약 및 검색",
    apiType: "rest",
    category: "travel",
  },
  {
    id: "booking-api",
    name: "Booking.com API",
    description: "Hotel and accommodation search and booking",
    descriptionKo: "호텔 및 숙박 검색 및 예약",
    apiType: "rest",
    category: "travel",
  },
  // --- Document / OCR ---
  {
    id: "google-docs-api",
    name: "Google Docs API",
    description: "Create and edit Google Docs programmatically",
    descriptionKo: "Google Docs 프로그래밍 방식 생성 및 편집",
    apiType: "rest",
    category: "document",
  },
  {
    id: "google-slides-api",
    name: "Google Slides API",
    description: "Create and manipulate presentations",
    descriptionKo: "프레젠테이션 생성 및 조작",
    apiType: "rest",
    category: "document",
  },
  {
    id: "ms-word-api",
    name: "Microsoft Word (Graph API)",
    description: "Word document creation and editing via Graph API",
    descriptionKo: "Graph API를 통한 Word 문서 생성 및 편집",
    apiType: "rest",
    category: "document",
  },
  {
    id: "pdf-co",
    name: "PDF.co API",
    description: "PDF generation, conversion, merge, split, OCR",
    descriptionKo: "PDF 생성, 변환, 병합, 분할, OCR",
    apiType: "rest",
    category: "document",
  },
  {
    id: "upstage-docparse",
    name: "Upstage Document Parser",
    description: "Advanced OCR for Korean/English documents, tables, forms",
    descriptionKo: "한/영 문서, 표, 양식 고급 OCR",
    apiType: "rest",
    category: "document",
  },
  {
    id: "google-vision",
    name: "Google Cloud Vision",
    description: "OCR, image labeling, face/object detection",
    descriptionKo: "OCR, 이미지 라벨링, 얼굴/객체 감지",
    apiType: "rest",
    category: "document",
  },
  {
    id: "aws-textract",
    name: "AWS Textract",
    description: "Document text extraction, table/form detection",
    descriptionKo: "문서 텍스트 추출, 표/양식 감지",
    apiType: "rest",
    category: "document",
  },
  // --- Coding / Dev ---
  {
    id: "docker-sandbox",
    name: "Docker Sandbox",
    description: "Isolated code execution environment",
    descriptionKo: "격리된 코드 실행 환경",
    apiType: "sdk",
    category: "coding",
  },
  {
    id: "replit-api",
    name: "Replit API",
    description: "Cloud IDE for code execution and collaboration",
    descriptionKo: "코드 실행 및 협업을 위한 클라우드 IDE",
    apiType: "rest",
    category: "coding",
  },
  {
    id: "figma-api",
    name: "Figma REST API",
    description: "Design file access, component creation, style management",
    descriptionKo: "디자인 파일 접근, 컴포넌트 생성, 스타일 관리",
    apiType: "rest",
    category: "design",
  },
  {
    id: "github-api",
    name: "GitHub API",
    description: "Repository, PR, issue management and CI/CD",
    descriptionKo: "리포지토리, PR, 이슈 관리 및 CI/CD",
    apiType: "rest",
    category: "coding",
  },
  {
    id: "supabase-api",
    name: "Supabase API",
    description: "Database, auth, storage, realtime as a service",
    descriptionKo: "서비스형 데이터베이스, 인증, 스토리지, 실시간",
    apiType: "rest",
    category: "database",
  },
  {
    id: "neon-api",
    name: "Neon API",
    description: "Serverless PostgreSQL with branching and autoscaling",
    descriptionKo: "서버리스 PostgreSQL, 브랜칭 및 자동 스케일링",
    apiType: "rest",
    category: "database",
  },
  {
    id: "terraform-cli",
    name: "Terraform CLI",
    description: "Infrastructure as Code provisioning and management",
    descriptionKo: "코드형 인프라 프로비저닝 및 관리",
    apiType: "sdk",
    category: "infra",
  },
  // --- Music ---
  {
    id: "suno-api",
    name: "Suno API",
    description: "AI music generation with lyrics, genre, and cover art",
    descriptionKo: "가사, 장르, 커버 아트 포함 AI 음악 생성",
    apiType: "rest",
    category: "music",
  },
  {
    id: "lyria-realtime",
    name: "Lyria RealTime (Gemini API)",
    description: "Real-time interactive music generation and editing",
    descriptionKo: "실시간 인터랙티브 음악 생성 및 편집",
    apiType: "realtime",
    category: "music",
  },
  {
    id: "lyria-3",
    name: "Lyria 3",
    description: "High-quality vocal and instrumental music generation",
    descriptionKo: "고품질 보컬 및 기악 음악 생성",
    apiType: "rest",
    category: "music",
  },
  // --- Image ---
  {
    id: "seedance-image",
    name: "Seedance Image",
    description: "Photorealistic image generation, upscaling, character consistency",
    descriptionKo: "실사 이미지 생성, 업스케일링, 캐릭터 일관성",
    apiType: "rest",
    category: "image",
  },
  {
    id: "nanobanana",
    name: "Nano Banana",
    description: "Stylized illustration, cel anime, artistic image generation",
    descriptionKo: "스타일리시 일러스트, 셀 애니, 예술적 이미지 생성",
    apiType: "rest",
    category: "image",
  },
  {
    id: "freepik-api",
    name: "Freepik API",
    description: "Stock images, vectors, icons, AI image generation & upscale",
    descriptionKo: "스톡 이미지, 벡터, 아이콘, AI 이미지 생성 및 업스케일",
    apiType: "rest",
    category: "image",
  },
  {
    id: "dall-e-3",
    name: "DALL-E 3",
    description: "OpenAI image generation with detailed prompt following",
    descriptionKo: "상세 프롬프트 준수 이미지 생성",
    apiType: "rest",
    category: "image",
  },
  {
    id: "midjourney-api",
    name: "Midjourney API",
    description: "High-quality artistic and photorealistic image generation",
    descriptionKo: "고품질 예술적/실사 이미지 생성",
    apiType: "rest",
    category: "image",
  },
  {
    id: "stable-diffusion",
    name: "Stable Diffusion API",
    description: "Open-source image generation with fine-tuning support",
    descriptionKo: "파인튜닝 지원 오픈소스 이미지 생성",
    apiType: "rest",
    category: "image",
  },
  // --- Video ---
  {
    id: "seedance-video",
    name: "Seedance 2.0 Video API",
    description: "Text/image/video to video generation, dance/motion reference",
    descriptionKo: "텍스트/이미지/비디오 투 비디오 생성, 안무/모션 레퍼런스",
    apiType: "rest",
    category: "video",
  },
  {
    id: "youtube-data-api",
    name: "YouTube Data API",
    description: "Video upload, metadata management, analytics",
    descriptionKo: "비디오 업로드, 메타데이터 관리, 분석",
    apiType: "rest",
    category: "video",
  },
  {
    id: "runway-api",
    name: "Runway Gen-3 API",
    description: "AI video generation and editing with motion control",
    descriptionKo: "모션 제어 포함 AI 비디오 생성 및 편집",
    apiType: "rest",
    category: "video",
  },
  {
    id: "kling-api",
    name: "Kling AI API",
    description: "High-quality video generation with Chinese/Asian content strength",
    descriptionKo: "고품질 비디오 생성, 아시아 콘텐츠에 강점",
    apiType: "rest",
    category: "video",
  },
  // --- Communication / Payment ---
  {
    id: "gmail-api",
    name: "Gmail API",
    description: "Email send, receive, search, and management",
    descriptionKo: "이메일 전송, 수신, 검색 및 관리",
    apiType: "rest",
    category: "communication",
  },
  {
    id: "google-drive-api",
    name: "Google Drive API",
    description: "File storage, sharing, and management",
    descriptionKo: "파일 저장, 공유 및 관리",
    apiType: "rest",
    category: "storage",
  },
  {
    id: "toss-payments",
    name: "Toss Payments API",
    description: "Korean payment gateway for card, transfer, virtual account",
    descriptionKo: "카드, 계좌이체, 가상계좌 한국 결제 게이트웨이",
    apiType: "rest",
    category: "payment",
  },
  {
    id: "open-banking",
    name: "Open Banking API",
    description: "Korean open banking for account info and transfers",
    descriptionKo: "한국 오픈뱅킹 계좌 정보 및 이체",
    apiType: "rest",
    category: "payment",
  },
];

// ============================================================
// Channels Registry
// ============================================================
export const channels: ChannelOption[] = [
  { id: "web", name: "Web Chat", icon: "globe", description: "Browser-based chat interface" },
  { id: "kakao", name: "KakaoTalk", icon: "message-circle", description: "Korean messenger integration" },
  { id: "telegram", name: "Telegram", icon: "send", description: "Telegram bot channel" },
  { id: "discord", name: "Discord", icon: "hash", description: "Discord bot channel" },
  { id: "slack", name: "Slack", icon: "slack", description: "Slack workspace integration" },
  { id: "whatsapp", name: "WhatsApp", icon: "phone", description: "WhatsApp Business API" },
  { id: "signal", name: "Signal", icon: "shield", description: "Signal messenger channel" },
  { id: "email", name: "Email", icon: "mail", description: "Email-based interaction" },
  { id: "sms", name: "SMS", icon: "smartphone", description: "SMS text messaging" },
  { id: "feishu", name: "Feishu/Lark", icon: "feather", description: "Feishu/Lark integration" },
];

// ============================================================
// Domain Definitions with Sub-Agents
// ============================================================
export const domains: Domain[] = [
  {
    id: "web-shopping",
    name: "Web / Shopping",
    nameKo: "웹/쇼핑",
    icon: "shopping-cart",
    color: "primary",
    subAgents: [
      {
        id: "personal-shopping",
        name: "Personal Shopping Planner",
        nameKo: "개인 쇼핑 플래너",
        description: "Compare prices, track deals, build smart shopping lists",
        descriptionKo: "가격 비교, 딜 추적, 스마트 쇼핑 리스트 생성",
        recommendedLLMs: ["gemini-2.5-flash", "claude-sonnet-4"],
        recommendedTools: ["playwright", "google-maps"],
        alternativeLLMs: ["gpt-4.1-mini", "deepseek-v3"],
        alternativeTools: ["puppeteer", "browserless"],
      },
      {
        id: "product-management",
        name: "Product Photo/Price/Stock Manager",
        nameKo: "상품 사진/가격/재고 관리",
        description: "Sync products across platforms, auto-upload images and descriptions",
        descriptionKo: "플랫폼 간 상품 동기화, 이미지/설명 자동 업로드",
        recommendedLLMs: ["claude-sonnet-4", "gemini-2.5-flash"],
        recommendedTools: ["playwright", "shopify-api", "seedance-image"],
        alternativeLLMs: ["gpt-4.1-mini", "deepseek-v3"],
        alternativeTools: ["cafe24-api", "coupang-api", "freepik-api"],
      },
      {
        id: "review-management",
        name: "Review Reply Auto-Manager",
        nameKo: "쇼핑몰 후기 답글 관리",
        description: "Collect reviews, sentiment analysis, auto-draft replies",
        descriptionKo: "후기 수집, 감성 분석, 답글 초안 자동 작성",
        recommendedLLMs: ["gemini-2.5-pro", "claude-opus-4.6"],
        recommendedTools: ["playwright"],
        alternativeLLMs: ["claude-sonnet-4", "gpt-4.1"],
        alternativeTools: ["puppeteer", "browserless"],
      },
    ],
  },
  {
    id: "daily-assistant",
    name: "Daily / Assistant",
    nameKo: "일상/비서",
    icon: "calendar",
    color: "secondary",
    subAgents: [
      {
        id: "life-agent",
        name: "Life Agent",
        nameKo: "라이프 에이전트",
        description: "Calendar, meetings, to-do, contacts, photos auto-management",
        descriptionKo: "캘린더, 회의, To-do, 연락처, 사진 자동 관리",
        recommendedLLMs: ["gemini-2.5-flash", "claude-sonnet-4"],
        recommendedTools: ["google-calendar", "notion-api", "google-drive-api"],
        alternativeLLMs: ["gpt-4.1-mini", "deepseek-v3"],
        alternativeTools: ["google-tasks", "todoist-api", "ms-graph", "google-vision"],
      },
      {
        id: "execution-agent",
        name: "Bill Payment Executor",
        nameKo: "집행 에이전트",
        description: "Utility bills, taxes, recurring payments automation (with approval)",
        descriptionKo: "공과금, 세금, 정기결제 자동화 (승인 후 실행)",
        recommendedLLMs: ["claude-opus-4.6", "gemini-2.5-pro"],
        recommendedTools: ["playwright", "gmail-api"],
        alternativeLLMs: ["claude-sonnet-4", "gpt-4.1"],
        alternativeTools: ["browserless", "toss-payments", "open-banking"],
      },
      {
        id: "travel-agent",
        name: "Travel / Leisure Agent",
        nameKo: "여행/레저 에이전트",
        description: "Plan trips, book flights/hotels, optimize itineraries",
        descriptionKo: "여행 계획, 항공/호텔 예약, 일정 최적화",
        recommendedLLMs: ["gemini-2.5-pro", "claude-sonnet-4"],
        recommendedTools: ["skyscanner-api", "google-maps", "amadeus-api"],
        alternativeLLMs: ["gpt-4o", "deepseek-v3"],
        alternativeTools: ["booking-api", "playwright"],
      },
      {
        id: "navigation-assistant",
        name: "Navigation Assistant",
        nameKo: "길찾기 어시스턴트",
        description: "Real-time navigation, route optimization, traffic info",
        descriptionKo: "실시간 네비게이션, 경로 최적화, 교통 정보",
        recommendedLLMs: ["gemini-2.5-flash"],
        recommendedTools: ["google-maps"],
        alternativeLLMs: ["gpt-4.1-mini", "claude-haiku-3.5"],
        alternativeTools: [],
      },
    ],
  },
  {
    id: "document",
    name: "Document Work",
    nameKo: "문서작업",
    icon: "file-text",
    color: "accent",
    subAgents: [
      {
        id: "doc-creation",
        name: "Document Creation",
        nameKo: "일반 문서작업",
        description: "Create Word, Excel, PDF documents from scratch or templates",
        descriptionKo: "워드, 엑셀, PDF 문서 생성 (처음부터 또는 템플릿)",
        recommendedLLMs: ["claude-opus-4.6", "gemini-2.5-pro"],
        recommendedTools: ["google-docs-api", "ms-word-api", "pdf-co"],
        alternativeLLMs: ["gpt-4.1", "deepseek-v3"],
        alternativeTools: ["notion-api"],
      },
      {
        id: "doc-conversion-ocr",
        name: "Document Conversion / OCR",
        nameKo: "문서변환 / OCR",
        description: "Convert formats, OCR scanned/image documents",
        descriptionKo: "포맷 변환, 스캔/이미지 문서 OCR 처리",
        recommendedLLMs: ["upstage-solar", "gemini-2.5-pro"],
        recommendedTools: ["upstage-docparse", "pdf-co"],
        alternativeLLMs: ["claude-opus-4.6", "gpt-4o"],
        alternativeTools: ["google-vision", "aws-textract"],
      },
      {
        id: "doc-summary-ppt",
        name: "Summary / Presentation",
        nameKo: "문서요약 / 프레젠테이션",
        description: "Summarize documents and auto-generate presentations",
        descriptionKo: "문서 요약 및 프레젠테이션 자동 생성",
        recommendedLLMs: ["claude-opus-4.6", "gemini-2.5-pro"],
        recommendedTools: ["google-slides-api", "google-docs-api"],
        alternativeLLMs: ["gpt-4.1", "claude-sonnet-4"],
        alternativeTools: ["ms-word-api", "pdf-co"],
      },
    ],
  },
  {
    id: "coding",
    name: "Coding / Dev",
    nameKo: "코딩/개발",
    icon: "code",
    color: "primary",
    subAgents: [
      {
        id: "self-coding",
        name: "Self Coding-Debugging Agent",
        nameKo: "셀프 코딩-디버깅 에이전트",
        description: "Replit-style: requirements to code, test, debug loop",
        descriptionKo: "Replit 스타일: 요구사항에서 코드, 테스트, 디버그 루프",
        recommendedLLMs: ["claude-opus-4.6", "gemini-2.5-pro"],
        recommendedTools: ["docker-sandbox", "playwright", "github-api"],
        alternativeLLMs: ["gpt-4.1", "deepseek-r1", "o3"],
        alternativeTools: ["replit-api"],
      },
      {
        id: "web-design",
        name: "Figma + Web Design Agent",
        nameKo: "Figma + 웹디자인 에이전트",
        description: "Design to code: Figma designs to React/Next.js components",
        descriptionKo: "디자인 투 코드: Figma 디자인을 React/Next.js 컴포넌트로",
        recommendedLLMs: ["claude-opus-4.6", "gemini-2.5-pro"],
        recommendedTools: ["figma-api", "docker-sandbox"],
        alternativeLLMs: ["gpt-4o", "claude-sonnet-4"],
        alternativeTools: ["github-api"],
      },
      {
        id: "infra-designer",
        name: "Infra / Agent Graph Designer",
        nameKo: "인프라/에이전트 그래프 디자이너",
        description: "Design agent graphs, IaC scripts, deployment pipelines",
        descriptionKo: "에이전트 그래프, IaC 스크립트, 배포 파이프라인 설계",
        recommendedLLMs: ["claude-opus-4.6", "gemini-2.5-pro"],
        recommendedTools: ["github-api", "terraform-cli"],
        alternativeLLMs: ["gpt-4.1", "deepseek-r1"],
        alternativeTools: ["docker-sandbox"],
      },
      {
        id: "db-agent",
        name: "Database Creation Agent",
        nameKo: "DB 생성 에이전트",
        description: "Schema design, migration SQL, ORM model generation",
        descriptionKo: "스키마 설계, 마이그레이션 SQL, ORM 모델 생성",
        recommendedLLMs: ["claude-opus-4.6", "gemini-2.5-pro"],
        recommendedTools: ["supabase-api", "neon-api"],
        alternativeLLMs: ["gpt-4.1", "deepseek-r1"],
        alternativeTools: ["docker-sandbox"],
      },
    ],
  },
  {
    id: "interpretation",
    name: "Interpretation",
    nameKo: "통역",
    icon: "languages",
    color: "secondary",
    subAgents: [
      {
        id: "live-interpreter",
        name: "Real-time Interpretation Copilot",
        nameKo: "실시간 통역/회의 코파일럿",
        description: "Voice input to real-time translation, meeting notes, action items",
        descriptionKo: "음성 입력에서 실시간 통역, 회의록, 액션 아이템 생성",
        recommendedLLMs: ["gemini-2.5-flash-live", "gemini-2.5-pro"],
        recommendedTools: ["notion-api", "google-docs-api"],
        alternativeLLMs: ["gpt-4o", "claude-opus-4.6"],
        alternativeTools: ["ms-graph", "google-drive-api"],
      },
    ],
  },
  {
    id: "music",
    name: "Music",
    nameKo: "음악",
    icon: "music",
    color: "accent",
    subAgents: [
      {
        id: "music-production",
        name: "Music / Lyrics / Arrangement Agent",
        nameKo: "음악/가사/편곡 에이전트",
        description: "Concept to song: lyrics, demo, arrangement, cover art pipeline",
        descriptionKo: "컨셉에서 곡까지: 가사, 데모, 편곡, 커버 아트 파이프라인",
        recommendedLLMs: ["claude-opus-4.6", "gemini-2.5-pro"],
        recommendedTools: ["suno-api", "lyria-realtime"],
        alternativeLLMs: ["gpt-4.1", "claude-sonnet-4"],
        alternativeTools: ["lyria-3"],
      },
    ],
  },
  {
    id: "image",
    name: "Image",
    nameKo: "이미지",
    icon: "image",
    color: "primary",
    subAgents: [
      {
        id: "seedance-image-gen",
        name: "Seedance Image Studio",
        nameKo: "Seedance 이미지 생성/보정",
        description: "Photorealistic images, product shots, character-consistent visuals",
        descriptionKo: "실사 이미지, 상품 촬영, 캐릭터 일관성 비주얼",
        recommendedLLMs: ["gemini-2.5-pro", "claude-sonnet-4"],
        recommendedTools: ["seedance-image"],
        alternativeLLMs: ["gpt-4o"],
        alternativeTools: ["dall-e-3", "midjourney-api", "stable-diffusion"],
      },
      {
        id: "nanobanana-gen",
        name: "Nano Banana Studio",
        nameKo: "나노바나나 이미지 생성/보정",
        description: "Stylized illustrations, anime, artistic thumbnails",
        descriptionKo: "스타일리시 일러스트, 애니메이션, 예술적 썸네일",
        recommendedLLMs: ["gemini-2.5-pro", "claude-sonnet-4"],
        recommendedTools: ["nanobanana"],
        alternativeLLMs: ["gpt-4o"],
        alternativeTools: ["seedance-image", "midjourney-api", "stable-diffusion"],
      },
      {
        id: "freepik-gen",
        name: "Freepik Design Studio",
        nameKo: "Freepik 이미지 생성/업스케일",
        description: "Stock images, vectors, icons, infographics, upscaling",
        descriptionKo: "스톡 이미지, 벡터, 아이콘, 인포그래픽, 업스케일링",
        recommendedLLMs: ["gemini-2.5-flash", "claude-haiku-3.5"],
        recommendedTools: ["freepik-api"],
        alternativeLLMs: ["gpt-4.1-mini"],
        alternativeTools: ["seedance-image", "dall-e-3"],
      },
    ],
  },
  {
    id: "video",
    name: "Video",
    nameKo: "비디오",
    icon: "video",
    color: "secondary",
    subAgents: [
      {
        id: "shortform-studio",
        name: "YouTube Shorts Studio",
        nameKo: "유튜브 숏폼 스튜디오",
        description: "Topic research to script, shots, Seedance render, thumbnail, upload",
        descriptionKo: "주제 리서치에서 스크립트, 샷, Seedance 렌더, 썸네일, 업로드까지",
        recommendedLLMs: ["claude-opus-4.6", "gemini-2.5-pro"],
        recommendedTools: ["seedance-video", "youtube-data-api", "seedance-image"],
        alternativeLLMs: ["gpt-4.1", "claude-sonnet-4"],
        alternativeTools: ["runway-api", "kling-api", "nanobanana", "freepik-api"],
      },
      {
        id: "ai-idol-mv",
        name: "AI Idol / Music Video Pipeline",
        nameKo: "AI 아이돌/뮤직비디오 프로덕션",
        description: "Character world-building, song release, MV/dance/teaser/shorts",
        descriptionKo: "캐릭터/세계관, 신곡 릴리즈, MV/안무/티저/쇼츠 파이프라인",
        recommendedLLMs: ["claude-opus-4.6", "gemini-2.5-pro"],
        recommendedTools: ["seedance-video", "suno-api", "seedance-image"],
        alternativeLLMs: ["gpt-4o", "gpt-4.1"],
        alternativeTools: ["lyria-realtime", "runway-api", "kling-api", "nanobanana"],
      },
    ],
  },
];

// ============================================================
// Helper functions
// ============================================================
export function getLLMById(id: string): LLMModel | undefined {
  return llmModels.find((m) => m.id === id);
}

export function getToolById(id: string): ToolAPI | undefined {
  return toolAPIs.find((t) => t.id === id);
}

export function getChannelById(id: string): ChannelOption | undefined {
  return channels.find((c) => c.id === id);
}

export function getDomainById(id: string): Domain | undefined {
  return domains.find((d) => d.id === id);
}

export function getSubAgentById(domainId: string, subAgentId: string): SubAgent | undefined {
  const domain = getDomainById(domainId);
  return domain?.subAgents.find((s) => s.id === subAgentId);
}

export function getAllLLMsForSubAgent(subAgent: SubAgent): LLMModel[] {
  const allIds = [...subAgent.recommendedLLMs, ...subAgent.alternativeLLMs];
  return allIds.map(getLLMById).filter((m): m is LLMModel => m !== undefined);
}

export function getAllToolsForSubAgent(subAgent: SubAgent): ToolAPI[] {
  const allIds = [...subAgent.recommendedTools, ...subAgent.alternativeTools];
  return allIds.map(getToolById).filter((t): t is ToolAPI => t !== undefined);
}
