import Link from "next/link";
import Footer from "@/components/Footer";

const keyAdvantages = [
  {
    icon: "\uD83D\uDD12",
    title: "\uCCA0\uD1B5 \uBCF4\uC548",
    titleEn: "Military-Grade Security",
    description:
      "ChaCha20-Poly1305 + AES-256-GCM \uC774\uC911 \uC554\uD638\uD654, \uBE14\uB8E8\uD22C\uC2A4 \uBC29\uC2DD \uD398\uC5B4\uB9C1 \uC778\uC99D, 256\uBE44\uD2B8 \uBCA0\uC5B4\uB7EC \uD1A0\uD070, \uBE0C\uB8E8\uD2B8\uD3EC\uC2A4 \uCC28\uB2E8(5\uD68C \uC2E4\uD328 \uC2DC \uC790\uB3D9 \uC7A0\uAE08). API \uD0A4\uBD80\uD130 \uB300\uD654 \uB0B4\uC6A9\uAE4C\uC9C0 \uBAA8\uB4E0 \uB370\uC774\uD130\uAC00 \uC554\uD638\uD654\uB429\uB2C8\uB2E4.",
    descriptionEn:
      "Dual encryption (ChaCha20 + AES-256), Bluetooth-style pairing auth, 256-bit bearer tokens, brute-force lockout. Every secret encrypted at rest.",
  },
  {
    icon: "\u26A1",
    title: "\uC6D0\uD074\uB9AD \uC124\uCE58",
    titleEn: "One-Click Install",
    description:
      "Windows(.msi), Mac(.dmg), Linux, Android(.apk) \u2014 \uD074\uB9AD \uD55C \uBC88\uC73C\uB85C \uC124\uCE58 \uC644\uB8CC. 3.4MB \uCD08\uACBD\uB7C9 \uBC14\uC774\uB108\uB9AC, 10ms \uC774\uB0B4 \uC2DC\uC791, \uBA54\uBAA8\uB9AC 5MB \uBBF8\uB9CC. Node.js\uB098 Python \uC124\uCE58 \uD544\uC694 \uC5C6\uC774 \uBC14\uB85C \uC2E4\uD589\uB429\uB2C8\uB2E4.",
    descriptionEn:
      "3.4MB binary, <10ms startup, <5MB RAM. No Node.js or Python needed. Just click and run.",
  },
  {
    icon: "\uD83D\uDCF1",
    title: "\uCD5C\uC18C 1\uB300\uBA74 OK",
    titleEn: "One Device Is All You Need",
    description:
      "\uB178\uD2B8\uBD81 1\uB300\uC5D0\uB9CC \uC124\uCE58\uD574\uB3C4 \uD734\uB300\uD3F0, \uD0DC\uBE14\uB9BF \uB4F1 \uB2E4\uB978 \uAE30\uAE30\uC5D0\uC11C \uC6F9 \uBE0C\uB77C\uC6B0\uC800\uB85C \uC811\uC18D\uD558\uC5EC \uC124\uCE58 \uC5C6\uC774 \uC0AC\uC6A9 \uAC00\uB2A5. \uBB3C\uB860 \uC5EC\uB7EC \uB300\uC5D0 \uC124\uCE58\uD574\uB3C4 \uB429\uB2C8\uB2E4.",
    descriptionEn:
      "Install on just one laptop \u2014 access from any phone or tablet via web browser. No extra installs needed.",
  },
  {
    icon: "\uD83D\uDD04",
    title: "\uC2E4\uC2DC\uAC04 \uB3D9\uAE30\uD654",
    titleEn: "Real-Time Sync",
    description:
      "\uB178\uD2B8\uBD81, \uD734\uB300\uD3F0, \uD0DC\uBE14\uB9BF \uC0AC\uC774\uC5D0\uC11C AI\uC758 \uC7A5\uAE30 \uAE30\uC5B5\uACFC \uB300\uD654 \uB0B4\uC6A9\uC774 \uC2E4\uC2DC\uAC04\uC73C\uB85C \uB3D9\uAE30\uD654\uB429\uB2C8\uB2E4. Cloudflare, Tailscale, ngrok \uD130\uB110 \uC9C0\uC6D0.",
    descriptionEn:
      "Memory, conversations, and settings sync across all your devices in real-time via encrypted tunnels.",
  },
  {
    icon: "\uD83C\uDFAF",
    title: "\uBB34\uD55C \uD65C\uC6A9",
    titleEn: "Unlimited Capabilities",
    description:
      "\uB0A0\uC528, \uC5EC\uD589, \uAE38\uC548\uB0B4, \uBB38\uC11C\uC791\uC5C5, \uC790\uB3D9\uCF54\uB529, \uD30C\uC77C \uC218\uC815, \uC6F9 \uBE0C\uB77C\uC6B0\uC9D5, \uC608\uC57D \uC791\uC5C5, \uD558\uB4DC\uC6E8\uC5B4 \uC81C\uC5B4\uAE4C\uC9C0. 25\uAC1C \uC774\uC0C1\uC758 \uB3C4\uAD6C\uB85C \uC5C4\uACA9\uD55C \uBCF4\uC548 \uB0B4\uC5D0\uC11C \uC5B4\uB5A4 \uC9C0\uC2DC\uB3C4 \uC218\uD589\uD569\uB2C8\uB2E4.",
    descriptionEn:
      "25+ tools: shell, browser, file management, coding, cron scheduling, hardware control \u2014 all within strict security policies.",
  },
  {
    icon: "\uD83D\uDCAC",
    title: "\uCE74\uCE74\uC624\uD1A1 \uC5F0\uB3D9",
    titleEn: "KakaoTalk Integration",
    description:
      "\uD734\uB300\uD3F0\uC5D0 \uC571 \uC124\uCE58 \uC5C6\uC774\uB3C4 \uCE74\uD1A1 \uBA54\uC2DC\uC9C0\uB9CC\uC73C\uB85C AI\uC640 \uBB34\uC81C\uD55C \uB300\uD654 \uBC0F \uC9C0\uC2DC \uAC00\uB2A5. Telegram, Discord, Slack, WhatsApp, Signal \uB4F1 18\uAC1C \uC774\uC0C1 \uCC44\uB110 \uC9C0\uC6D0.",
    descriptionEn:
      "Chat with AI via KakaoTalk without installing anything. Also supports Telegram, Discord, Slack, WhatsApp, Signal, and 18+ channels.",
  },
];

const detailedFeatures = [
  {
    icon: "\uD83E\uDD16",
    title: "23\uAC1C+ AI \uBAA8\uB378",
    titleEn: "23+ AI Models",
    description:
      "OpenAI, Anthropic Claude, Google Gemini, Ollama, OpenRouter, Azure, Together AI, LM Studio \uB4F1 \uD558\uB098\uC758 \uC778\uD130\uD398\uC774\uC2A4\uC5D0\uC11C \uC790\uC720\uB86D\uAC8C \uC804\uD658.",
    descriptionEn:
      "Switch freely between OpenAI, Claude, Gemini, Ollama, and 23+ providers from one interface.",
  },
  {
    icon: "\uD83E\uDDE0",
    title: "\uD559\uC2B5\uD615 \uC7A5\uAE30 \uAE30\uC5B5",
    titleEn: "Adaptive Long-Term Memory",
    description:
      "SQLite + \uBCA1\uD130 \uC784\uBCA0\uB529 \uAE30\uBC18 \uC2DC\uB9E8\uD2F1 \uAC80\uC0C9\uC73C\uB85C \uB300\uD654 \uB9E5\uB77D\uACFC \uC120\uD638\uB3C4\uB97C \uD559\uC2B5\uD558\uC5EC \uC810\uC810 \uB354 \uB611\uB611\uD574\uC9C0\uB294 AI.",
    descriptionEn:
      "Vector embeddings + SQLite for semantic search. Your AI remembers and learns over time.",
  },
  {
    icon: "\uD83D\uDEE1\uFE0F",
    title: "3\uB2E8\uACC4 \uBCF4\uC548 \uC815\uCC45",
    titleEn: "3-Layer Security Policy",
    description:
      "\uBA85\uB839\uC5B4 \uD654\uC774\uD2B8\uB9AC\uC2A4\uD2B8 + \uACBD\uB85C \uC811\uADFC \uC81C\uD55C + \uC561\uC158 \uC18D\uB3C4 \uC81C\uD55C(20\uD68C/\uBD84). 3-Strike \uC2DC\uC2A4\uD15C\uC73C\uB85C \uC774\uC0C1 \uD589\uB3D9 \uC790\uB3D9 \uCC28\uB2E8.",
    descriptionEn:
      "Command allowlist + path restrictions + rate limiting (20/min burst). 3-strike system auto-blocks abuse.",
  },
  {
    icon: "\uD83D\uDD27",
    title: "\uC790\uC728 \uB3C4\uAD6C \uC2E4\uD589",
    titleEn: "Autonomous Tool Execution",
    description:
      "\uD30C\uC77C \uC77D\uAE30/\uC4F0\uAE30, \uC258 \uBA85\uB839, Git, \uC6F9 \uBE0C\uB77C\uC6B0\uC9D5, \uC2A4\uD06C\uB9B0\uC0F7, HTTP \uC694\uCCAD, \uD558\uB4DC\uC6E8\uC5B4 \uC81C\uC5B4\uAE4C\uC9C0 25\uAC1C+ \uB3C4\uAD6C \uC790\uC728 \uC2E4\uD589.",
    descriptionEn:
      "25+ tools: file I/O, shell, Git, browser automation, screenshots, HTTP requests, and hardware control.",
  },
  {
    icon: "\u23F0",
    title: "\uC608\uC57D \uC791\uC5C5 (Cron)",
    titleEn: "Scheduled Tasks",
    description:
      "\uBC18\uBCF5 \uC791\uC5C5\uC744 \uC608\uC57D\uD558\uC5EC AI\uAC00 \uC790\uB3D9\uC73C\uB85C \uC2E4\uD589. \uB9E4\uC77C \uC544\uCE68 \uBB38\uC11C \uC815\uB9AC, \uC8FC\uAC04 \uBCF4\uACE0\uC11C \uC0DD\uC131 \uB4F1 \uBB34\uC778 \uC790\uB3D9\uD654.",
    descriptionEn:
      "Schedule recurring AI tasks: daily report generation, weekly summaries, automated maintenance.",
  },
  {
    icon: "\uD83D\uDCBB",
    title: "\uD06C\uB85C\uC2A4 \uD50C\uB7AB\uD3FC",
    titleEn: "Cross-Platform",
    description:
      "Windows, macOS, Linux, Android, iOS, \uC6F9 \uBE0C\uB77C\uC6B0\uC800 \u2014 \uBAA8\uB4E0 \uD50C\uB7AB\uD3FC\uC5D0\uC11C \uB3D9\uC77C\uD55C \uACBD\uD5D8. Tauri \uB370\uC2A4\uD06C\uD1B1 \uC571 + \uC6F9 \uCC44\uD305.",
    descriptionEn:
      "Same experience on Windows, macOS, Linux, Android, iOS, and Web. Tauri desktop + web chat.",
  },
];

const comparisonItems = [
  {
    category: "\uBCF4\uC548",
    categoryEn: "Security",
    openclaw: "\uD3C9\uBB38 \uD1A0\uD070 \uC800\uC7A5, \uAE30\uBCF8 \uC778\uC99D",
    moa: "ChaCha20 + AES-256 \uC774\uC911 \uC554\uD638\uD654, \uD398\uC5B4\uB9C1 \uC778\uC99D, \uBE0C\uB8E8\uD2B8\uD3EC\uC2A4 \uCC28\uB2E8",
  },
  {
    category: "\uC124\uCE58",
    categoryEn: "Install",
    openclaw: "Node.js + npm + \uD658\uACBD \uC124\uC815 \uD544\uC694",
    moa: "\uC6D0\uD074\uB9AD \uC124\uCE58 (3.4MB, \uCD94\uAC00 \uC758\uC874\uC131 \uC5C6\uC74C)",
  },
  {
    category: "\uBA54\uBAA8\uB9AC",
    categoryEn: "Memory",
    openclaw: "\uB2E8\uC21C \uD14D\uC2A4\uD2B8 \uD30C\uC77C \uC800\uC7A5",
    moa: "SQLite + \uBCA1\uD130 \uC784\uBCA0\uB529 \uC2DC\uB9E8\uD2F1 \uAC80\uC0C9 + \uB2E4\uB514\uBC14\uC774\uC2A4 \uB3D9\uAE30\uD654",
  },
  {
    category: "\uCC44\uB110",
    categoryEn: "Channels",
    openclaw: "CLI + Telegram \uC815\uB3C4",
    moa: "\uCE74\uCE74\uC624\uD1A1, Telegram, Discord, Slack, WhatsApp \uB4F1 18\uAC1C+",
  },
  {
    category: "\uBC14\uC774\uB108\uB9AC \uD06C\uAE30",
    categoryEn: "Binary Size",
    openclaw: "1GB+ (Node.js \uD3EC\uD568)",
    moa: "3.4MB (\uC815\uC801 \uBC14\uC774\uB108\uB9AC)",
  },
  {
    category: "\uC2DC\uC791 \uC2DC\uAC04",
    categoryEn: "Startup",
    openclaw: "\uC218 \uCD08",
    moa: "10ms \uC774\uB0B4",
  },
  {
    category: "\uC624\uD504\uB77C\uC778",
    categoryEn: "Offline",
    openclaw: "\uD074\uB77C\uC6B0\uB4DC \uD544\uC218",
    moa: "Ollama\uB85C \uC644\uC804 \uC624\uD504\uB77C\uC778 \uC6B4\uC601 \uAC00\uB2A5",
  },
  {
    category: "\uD558\uB4DC\uC6E8\uC5B4",
    categoryEn: "Hardware",
    openclaw: "\uBBF8\uC9C0\uC6D0",
    moa: "STM32, Raspberry Pi GPIO, ESP32 \uC81C\uC5B4",
  },
];

const pricingTiers = [
  {
    name: "Free",
    nameKo: "\uBB34\uB8CC",
    price: "\u20A90",
    priceNote: "\uC601\uC6D0\uD788 \uBB34\uB8CC forever free",
    features: [
      "\uAE30\uBCF8 AI \uBAA8\uB378 1\uAC1C Basic model (1)",
      "\uC6F9 \uCC44\uD305 \uBB34\uC81C\uD55C Unlimited web chat",
      "\uBA54\uBAA8\uB9AC 100MB Memory 100MB",
      "\uCEE4\uBBA4\uB2C8\uD2F0 \uC9C0\uC6D0 Community support",
    ],
    cta: "\uBB34\uB8CC\uB85C \uC2DC\uC791",
    ctaEn: "Start Free",
    href: "/chat",
    highlighted: false,
  },
  {
    name: "Pro",
    nameKo: "\uD504\uB85C",
    price: "\u20A99,900",
    priceNote: "/ \uC6D4 per month",
    features: [
      "\uBAA8\uB4E0 AI \uBAA8\uB378 (23\uAC1C+) All AI models",
      "\uBA40\uD2F0\uCC44\uB110 \uD1B5\uD569 (18\uAC1C+) Multi-channel",
      "\uBA54\uBAA8\uB9AC 10GB Memory 10GB",
      "\uB3C4\uAD6C \uC2E4\uD589 (25\uAC1C+) Tool execution",
      "\uC2E4\uC2DC\uAC04 \uB3D9\uAE30\uD654 Real-time sync",
      "\uCE74\uCE74\uC624\uD1A1 \uC5F0\uB3D9 KakaoTalk integration",
      "\uC6B0\uC120 \uC9C0\uC6D0 Priority support",
    ],
    cta: "\uD504\uB85C \uC2DC\uC791",
    ctaEn: "Start Pro",
    href: "/chat",
    highlighted: true,
  },
  {
    name: "Enterprise",
    nameKo: "\uC5D4\uD130\uD504\uB77C\uC774\uC988",
    price: "\uBB38\uC758",
    priceNote: "Contact us",
    features: [
      "\uBAA8\uB4E0 Pro \uAE30\uB2A5 All Pro features",
      "\uC804\uC6A9 \uC11C\uBC84 Dedicated server",
      "\uBB34\uC81C\uD55C \uBA54\uBAA8\uB9AC Unlimited memory",
      "SLA \uBCF4\uC7A5 SLA guarantee",
      "\uC804\uB2F4 \uB9E4\uB2C8\uC800 Dedicated manager",
      "\uCEE4\uC2A4\uD140 \uD1B5\uD569 Custom integration",
      "\uD558\uB4DC\uC6E8\uC5B4 \uC81C\uC5B4 Hardware control",
    ],
    cta: "\uC601\uC5C5\uD300 \uBB38\uC758",
    ctaEn: "Contact Sales",
    href: "mailto:enterprise@moa-agent.com",
    highlighted: false,
  },
];

export default function HomePage() {
  return (
    <>
      {/* Hero Section - OpenClaw Comparison */}
      <section className="relative overflow-hidden pt-32 pb-20 sm:pt-40 sm:pb-28">
        <div className="absolute inset-0 bg-hero-gradient" />
        <div className="absolute inset-0 bg-mesh-gradient" />
        <div className="absolute top-1/4 left-1/2 -translate-x-1/2 w-[600px] h-[600px] bg-primary-500/5 rounded-full blur-3xl" />

        <div className="relative mx-auto max-w-7xl px-4 sm:px-6 lg:px-8">
          <div className="text-center">
            {/* Badge */}
            <div className="mb-6 inline-flex items-center gap-2 rounded-full border border-primary-500/20 bg-primary-500/5 px-4 py-1.5 animate-fade-in">
              <div className="h-1.5 w-1.5 rounded-full bg-primary-400 animate-pulse" />
              <span className="text-xs font-medium text-primary-300">
                Powered by ZeroClaw Runtime &middot; 100% Rust
              </span>
            </div>

            {/* Main Headline */}
            <h1 className="text-3xl font-extrabold tracking-tight sm:text-5xl lg:text-6xl animate-fade-in-up">
              <span className="text-dark-200">{"\uD074\uB85C\uB4DC\uBD07(OpenClaw)\uC758 \uD55C\uACC4\uB97C \uB118\uC5B4,"}</span>
              <br />
              <span className="text-gradient text-4xl sm:text-6xl lg:text-7xl">ZeroClaw</span>
              <span className="text-dark-50 text-4xl sm:text-6xl lg:text-7xl">{"\uAC00 \uC654\uC2B5\uB2C8\uB2E4"}</span>
            </h1>

            {/* Sub-headline */}
            <p className="mx-auto mt-6 max-w-3xl text-base text-dark-300 leading-relaxed animate-fade-in-up sm:text-lg">
              {"\uC804 \uC138\uACC4 15\uB9CC\uBA85\uC774 \uC5F4\uAD11\uD55C \uD074\uB85C\uB4DC\uBD07! \uC124\uCE58\uAC00 \uC5B4\uB835\uACE0 \uBCF4\uC548\uC774 \uCDE8\uC57D\uD574\uC11C \uD3EC\uAE30\uD558\uC168\uB098\uC694?"}
              <br />
              <span className="text-primary-400 font-semibold">
                {"ZeroClaw\uB294 \uBCF4\uC548, \uC124\uCE58, \uC131\uB2A5, \uD65C\uC6A9\uC131 \uBAA8\uB4E0 \uBA74\uC5D0\uC11C \uC644\uC804\uD788 \uB2E4\uB985\uB2C8\uB2E4."}
              </span>
            </p>

            {/* CTA Buttons */}
            <div className="mt-10 flex flex-col items-center justify-center gap-4 sm:flex-row animate-fade-in-up">
              <Link href="/chat" className="btn-primary px-8 py-3.5 text-base glow-primary">
                {"\uC6F9\uC5D0\uC11C \uBC14\uB85C \uC2DC\uC791"}
                <svg className="ml-2 h-4 w-4" fill="none" viewBox="0 0 24 24" strokeWidth={2} stroke="currentColor">
                  <path strokeLinecap="round" strokeLinejoin="round" d="M13.5 4.5L21 12m0 0l-7.5 7.5M21 12H3" />
                </svg>
              </Link>
              <Link href="/download" className="btn-secondary px-8 py-3.5 text-base">
                {"\uC571 \uB2E4\uC6B4\uB85C\uB4DC"}
                <svg className="ml-2 h-4 w-4" fill="none" viewBox="0 0 24 24" strokeWidth={2} stroke="currentColor">
                  <path strokeLinecap="round" strokeLinejoin="round" d="M3 16.5v2.25A2.25 2.25 0 005.25 21h13.5A2.25 2.25 0 0021 18.75V16.5M16.5 12L12 16.5m0 0L7.5 12m4.5 4.5V3" />
                </svg>
              </Link>
            </div>

            {/* Stats */}
            <div className="mt-16 grid grid-cols-2 gap-6 sm:grid-cols-4 animate-fade-in-up">
              {[
                { value: "23+", label: "AI \uBAA8\uB378", labelEn: "AI Models" },
                { value: "18+", label: "\uCC44\uB110", labelEn: "Channels" },
                { value: "25+", label: "\uB3C4\uAD6C", labelEn: "Tools" },
                { value: "3.4MB", label: "\uBC14\uC774\uB108\uB9AC", labelEn: "Binary" },
              ].map((stat) => (
                <div key={stat.labelEn} className="text-center">
                  <div className="text-2xl font-bold text-gradient sm:text-3xl">{stat.value}</div>
                  <div className="mt-1 text-xs text-dark-400">
                    {stat.label} <span className="text-dark-600">{stat.labelEn}</span>
                  </div>
                </div>
              ))}
            </div>
          </div>
        </div>
      </section>

      {/* Key Advantages Section - 6 Core Strengths */}
      <section className="relative py-20 sm:py-28" id="advantages">
        <div className="absolute inset-0 bg-gradient-radial" />
        <div className="relative mx-auto max-w-7xl px-4 sm:px-6 lg:px-8">
          <div className="text-center mb-16">
            <h2 className="text-3xl font-bold text-dark-50 sm:text-4xl">
              {"ZeroClaw\uB9CC\uC758 6\uB300 \uD575\uC2EC \uAC15\uC810"}
              <span className="text-dark-500 ml-3 text-xl font-normal">Why ZeroClaw?</span>
            </h2>
            <p className="mt-4 text-dark-400 max-w-2xl mx-auto">
              {"\uD074\uB85C\uB4DC\uBD07\uC744 \uD3EC\uAE30\uD588\uB358 \uC774\uC720\uB4E4, ZeroClaw\uAC00 \uBAA8\uB450 \uD574\uACB0\uD588\uC2B5\uB2C8\uB2E4."}
            </p>
          </div>

          <div className="grid grid-cols-1 gap-6 sm:grid-cols-2 lg:grid-cols-3">
            {keyAdvantages.map((item, index) => (
              <div
                key={item.titleEn}
                className="glass-card rounded-2xl p-6 transition-all duration-300 hover:translate-y-[-2px]"
                style={{ animationDelay: `${index * 100}ms` }}
              >
                <div className="mb-4 flex h-12 w-12 items-center justify-center rounded-xl bg-primary-500/10 border border-primary-500/20 text-2xl">
                  {item.icon}
                </div>
                <h3 className="text-lg font-semibold text-dark-50 mb-1">{item.title}</h3>
                <p className="text-xs text-primary-400/60 font-medium mb-3">{item.titleEn}</p>
                <p className="text-sm text-dark-400 leading-relaxed">{item.description}</p>
                <p className="text-xs text-dark-500 mt-2 leading-relaxed">{item.descriptionEn}</p>
              </div>
            ))}
          </div>
        </div>
      </section>

      {/* Comparison Table - ZeroClaw vs OpenClaw */}
      <section className="py-20 sm:py-28" id="comparison">
        <div className="mx-auto max-w-4xl px-4 sm:px-6 lg:px-8">
          <div className="text-center mb-12">
            <h2 className="text-3xl font-bold text-dark-50 sm:text-4xl">
              {"ZeroClaw vs \uD074\uB85C\uB4DC\uBD07"}
              <span className="text-dark-500 ml-3 text-xl font-normal">Comparison</span>
            </h2>
            <p className="mt-4 text-dark-400">
              {"\uD55C\uB208\uC5D0 \uBCF4\uB294 \uCC28\uC774\uC810"}
            </p>
          </div>

          <div className="glass-card rounded-2xl overflow-hidden">
            {/* Table header */}
            <div className="grid grid-cols-3 bg-dark-800/80 border-b border-dark-700/50">
              <div className="px-4 py-3 sm:px-6">
                <span className="text-xs font-semibold text-dark-400">{"\uBE44\uAD50 \uD56D\uBAA9"}</span>
              </div>
              <div className="px-4 py-3 sm:px-6 text-center">
                <span className="text-xs font-semibold text-dark-500">{"\uD074\uB85C\uB4DC\uBD07"} (OpenClaw)</span>
              </div>
              <div className="px-4 py-3 sm:px-6 text-center">
                <span className="text-xs font-bold text-primary-400">ZeroClaw</span>
              </div>
            </div>

            {/* Table rows */}
            {comparisonItems.map((item, index) => (
              <div
                key={item.categoryEn}
                className={`grid grid-cols-3 ${index < comparisonItems.length - 1 ? "border-b border-dark-800/50" : ""}`}
              >
                <div className="px-4 py-3 sm:px-6 flex items-center">
                  <div>
                    <span className="text-sm font-medium text-dark-200">{item.category}</span>
                    <span className="text-[10px] text-dark-500 ml-1.5">{item.categoryEn}</span>
                  </div>
                </div>
                <div className="px-4 py-3 sm:px-6 flex items-center justify-center">
                  <span className="text-xs text-dark-500 text-center">{item.openclaw}</span>
                </div>
                <div className="px-4 py-3 sm:px-6 flex items-center justify-center bg-primary-500/[0.03]">
                  <span className="text-xs text-primary-300 text-center font-medium">{item.moa}</span>
                </div>
              </div>
            ))}
          </div>
        </div>
      </section>

      {/* Detailed Features Section */}
      <section className="relative py-20 sm:py-28" id="features">
        <div className="absolute inset-0 bg-gradient-radial" />
        <div className="relative mx-auto max-w-7xl px-4 sm:px-6 lg:px-8">
          <div className="text-center mb-16">
            <h2 className="text-3xl font-bold text-dark-50 sm:text-4xl">
              {"\uC0C1\uC138 \uAE30\uB2A5"}
              <span className="text-dark-500 ml-3 text-xl font-normal">Features</span>
            </h2>
            <p className="mt-4 text-dark-400 max-w-xl mx-auto">
              {"\uC790\uC728 AI \uC5D0\uC774\uC804\uD2B8\uC5D0 \uD544\uC694\uD55C \uBAA8\uB4E0 \uAE30\uB2A5\uC744 \uC81C\uACF5\uD569\uB2C8\uB2E4."}
            </p>
          </div>

          <div className="grid grid-cols-1 gap-6 sm:grid-cols-2 lg:grid-cols-3">
            {detailedFeatures.map((feature, index) => (
              <div
                key={feature.titleEn}
                className="glass-card rounded-2xl p-6 transition-all duration-300 hover:translate-y-[-2px]"
                style={{ animationDelay: `${index * 100}ms` }}
              >
                <div className="mb-4 flex h-12 w-12 items-center justify-center rounded-xl bg-primary-500/10 border border-primary-500/20 text-2xl">
                  {feature.icon}
                </div>
                <h3 className="text-lg font-semibold text-dark-50 mb-1">{feature.title}</h3>
                <p className="text-xs text-primary-400/60 font-medium mb-3">{feature.titleEn}</p>
                <p className="text-sm text-dark-400 leading-relaxed">{feature.description}</p>
                <p className="text-xs text-dark-500 mt-2 leading-relaxed">{feature.descriptionEn}</p>
              </div>
            ))}
          </div>
        </div>
      </section>

      {/* Architecture Section */}
      <section className="py-20 sm:py-28">
        <div className="mx-auto max-w-7xl px-4 sm:px-6 lg:px-8">
          <div className="grid grid-cols-1 gap-12 lg:grid-cols-2 items-center">
            <div>
              <h2 className="text-3xl font-bold text-dark-50 sm:text-4xl">ZeroClaw Runtime</h2>
              <p className="mt-4 text-dark-400 leading-relaxed">
                {"\uACE0\uC131\uB2A5 Rust \uB7F0\uD0C0\uC784 \uC704\uC5D0 \uAD6C\uCD95\uB41C ZeroClaw\uB294 \uBE60\uB978 \uC18D\uB3C4, \uB0AE\uC740 \uBA54\uBAA8\uB9AC, \uADF8\uB9AC\uACE0 \uAD70\uC0AC \uAE09 \uBCF4\uC548\uC744 \uB3D9\uC2DC\uC5D0 \uC81C\uACF5\uD569\uB2C8\uB2E4."}
              </p>
              <p className="mt-2 text-sm text-dark-500">
                Built on Rust: speed, minimal memory, and military-grade security in one binary.
              </p>
              <ul className="mt-8 space-y-4">
                {[
                  {
                    title: "\uC774\uC911 \uC554\uD638\uD654 \uC544\uD0A4\uD14D\uCC98",
                    titleEn: "Dual encryption architecture",
                    desc: "ChaCha20-Poly1305 (AEAD) + AES-256-GCM \u2014 API \uD0A4, \uD1A0\uD070, \uB300\uD654 \uBAA8\uB450 \uC554\uD638\uD654",
                  },
                  {
                    title: "\uD398\uC5B4\uB9C1 \uAE30\uBC18 \uC778\uC99D",
                    titleEn: "Bluetooth-style pairing auth",
                    desc: "6\uC790\uB9AC \uD398\uC5B4\uB9C1 \uCF54\uB4DC + 256\uBE44\uD2B8 \uBCA0\uC5B4\uB7EC \uD1A0\uD070 + \uBE0C\uB8E8\uD2B8\uD3EC\uC2A4 \uCC28\uB2E8",
                  },
                  {
                    title: "\uC81C\uB85C \uB7F0\uD0C0\uC784 \uC758\uC874\uC131",
                    titleEn: "Zero runtime dependencies",
                    desc: "Node.js, Python \uD544\uC694 \uC5C6\uC74C. 3.4MB \uC815\uC801 \uBC14\uC774\uB108\uB9AC\uB85C \uC5B4\uB514\uC11C\uB4E0 \uC2E4\uD589",
                  },
                  {
                    title: "\uD2B8\uB808\uC787 \uAE30\uBC18 \uBAA8\uB4C8\uB7EC \uD655\uC7A5",
                    titleEn: "Trait-driven modular extension",
                    desc: "\uD504\uB85C\uBC14\uC774\uB354, \uCC44\uB110, \uB3C4\uAD6C, \uBA54\uBAA8\uB9AC \uBAA8\uB450 \uD2B8\uB808\uC787\uC73C\uB85C \uC790\uC720\uB86D\uAC8C \uAD50\uCCB4/\uD655\uC7A5",
                  },
                ].map((item) => (
                  <li key={item.titleEn} className="flex gap-3">
                    <div className="mt-1 flex h-5 w-5 flex-shrink-0 items-center justify-center rounded-full bg-accent-500/10">
                      <svg className="h-3 w-3 text-accent-400" fill="none" viewBox="0 0 24 24" strokeWidth={3} stroke="currentColor">
                        <path strokeLinecap="round" strokeLinejoin="round" d="M4.5 12.75l6 6 9-13.5" />
                      </svg>
                    </div>
                    <div>
                      <span className="text-sm font-medium text-dark-200">{item.title}</span>
                      <span className="text-xs text-dark-500 ml-2">{item.titleEn}</span>
                      <p className="text-xs text-dark-400 mt-0.5">{item.desc}</p>
                    </div>
                  </li>
                ))}
              </ul>
            </div>

            {/* Architecture diagram */}
            <div className="glass-card rounded-2xl p-8">
              <div className="space-y-4">
                {[
                  {
                    label: "\uD504\uB85C\uBC14\uC774\uB354 Providers (23+)",
                    items: ["OpenAI", "Claude", "Gemini", "Ollama", "OpenRouter"],
                    color: "primary",
                  },
                  {
                    label: "\uCC44\uB110 Channels (18+)",
                    items: ["\uCE74\uCE74\uC624\uD1A1", "Telegram", "Discord", "Slack", "WhatsApp"],
                    color: "secondary",
                  },
                  {
                    label: "\uB3C4\uAD6C Tools (25+)",
                    items: ["Shell", "Browser", "File", "Git", "Cron"],
                    color: "accent",
                  },
                ].map((layer) => (
                  <div key={layer.label}>
                    <div className="text-xs font-medium text-dark-400 mb-2">{layer.label}</div>
                    <div className="grid grid-cols-5 gap-2">
                      {layer.items.map((item) => (
                        <div
                          key={item}
                          className={`rounded-lg border px-2 py-2 text-center text-[11px] font-medium transition-all hover:scale-[1.02] ${
                            layer.color === "primary"
                              ? "border-primary-500/20 bg-primary-500/5 text-primary-300"
                              : layer.color === "secondary"
                                ? "border-secondary-500/20 bg-secondary-500/5 text-secondary-300"
                                : "border-accent-500/20 bg-accent-500/5 text-accent-300"
                          }`}
                        >
                          {item}
                        </div>
                      ))}
                    </div>
                  </div>
                ))}

                {/* Security layer */}
                <div className="flex items-center justify-center py-2">
                  <div className="w-full rounded-xl border border-dark-600 bg-dark-800 px-4 py-3">
                    <div className="text-center">
                      <div className="text-sm font-bold text-gradient">ZeroClaw Core</div>
                      <div className="text-[10px] text-dark-500 mt-0.5">
                        Agent Loop | ChaCha20 + AES-256 | Pairing Auth | Rate Limiter
                      </div>
                    </div>
                  </div>
                </div>

                <div>
                  <div className="text-xs font-medium text-dark-400 mb-2">{"\uBA54\uBAA8\uB9AC Memory"}</div>
                  <div className="grid grid-cols-4 gap-2">
                    {["SQLite", "Vector", "Markdown", "Synced"].map((item) => (
                      <div
                        key={item}
                        className="rounded-lg border border-dark-600 bg-dark-800/50 px-2 py-2 text-center text-[11px] font-medium text-dark-300"
                      >
                        {item}
                      </div>
                    ))}
                  </div>
                </div>

                <div>
                  <div className="text-xs font-medium text-dark-400 mb-2">{"\uBCF4\uC548 Security"}</div>
                  <div className="grid grid-cols-3 gap-2">
                    {["\uC774\uC911 \uC554\uD638\uD654", "\uD398\uC5B4\uB9C1 \uC778\uC99D", "3-Strike \uCC28\uB2E8"].map((item) => (
                      <div
                        key={item}
                        className="rounded-lg border border-red-500/20 bg-red-500/5 px-2 py-2 text-center text-[11px] font-medium text-red-300"
                      >
                        {item}
                      </div>
                    ))}
                  </div>
                </div>
              </div>
            </div>
          </div>
        </div>
      </section>

      {/* Pricing Section */}
      <section className="relative py-20 sm:py-28" id="pricing">
        <div className="absolute inset-0 bg-gradient-radial" />
        <div className="relative mx-auto max-w-7xl px-4 sm:px-6 lg:px-8">
          <div className="text-center mb-16">
            <h2 className="text-3xl font-bold text-dark-50 sm:text-4xl">
              {"\uC694\uAE08\uC81C"}
              <span className="text-dark-500 ml-3 text-xl font-normal">Pricing</span>
            </h2>
            <p className="mt-4 text-dark-400 max-w-xl mx-auto">
              {"\uBB34\uB8CC\uB85C \uC2DC\uC791\uD558\uACE0, \uD544\uC694\uC5D0 \uB530\uB77C \uC5C5\uADF8\uB808\uC774\uB4DC\uD558\uC138\uC694."}
            </p>
            <p className="mt-1 text-sm text-dark-500">Start free, upgrade as you grow.</p>
          </div>

          <div className="grid grid-cols-1 gap-6 sm:grid-cols-2 lg:grid-cols-3 max-w-5xl mx-auto">
            {pricingTiers.map((tier) => (
              <div
                key={tier.name}
                className={`relative rounded-2xl p-6 transition-all duration-300 ${
                  tier.highlighted
                    ? "glass-card border-primary-500/30 bg-primary-500/5 scale-[1.02]"
                    : "glass-card"
                }`}
              >
                {tier.highlighted && (
                  <div className="absolute -top-3 left-1/2 -translate-x-1/2 rounded-full bg-primary-500 px-4 py-1 text-xs font-semibold text-white">
                    {"\uC778\uAE30"} Popular
                  </div>
                )}

                <div className="mb-6">
                  <h3 className="text-lg font-semibold text-dark-50">
                    {tier.nameKo}{" "}
                    <span className="text-dark-500 text-sm font-normal">{tier.name}</span>
                  </h3>
                  <div className="mt-3 flex items-baseline gap-1">
                    <span className="text-3xl font-bold text-dark-50">{tier.price}</span>
                    <span className="text-sm text-dark-500">{tier.priceNote}</span>
                  </div>
                </div>

                <ul className="space-y-3 mb-8">
                  {tier.features.map((feature) => (
                    <li key={feature} className="flex items-start gap-2.5">
                      <svg
                        className={`mt-0.5 h-4 w-4 flex-shrink-0 ${tier.highlighted ? "text-primary-400" : "text-dark-500"}`}
                        fill="none"
                        viewBox="0 0 24 24"
                        strokeWidth={2}
                        stroke="currentColor"
                      >
                        <path strokeLinecap="round" strokeLinejoin="round" d="M4.5 12.75l6 6 9-13.5" />
                      </svg>
                      <span className="text-sm text-dark-300">{feature}</span>
                    </li>
                  ))}
                </ul>

                <Link
                  href={tier.href}
                  className={`block w-full text-center rounded-lg px-6 py-3 text-sm font-semibold transition-all ${
                    tier.highlighted
                      ? "bg-primary-500 text-white hover:bg-primary-600"
                      : "border border-dark-600 bg-dark-800 text-dark-200 hover:border-dark-500 hover:bg-dark-700"
                  }`}
                >
                  {tier.cta} <span className="text-xs opacity-60">{tier.ctaEn}</span>
                </Link>
              </div>
            ))}
          </div>
        </div>
      </section>

      {/* CTA Section */}
      <section className="py-20 sm:py-28">
        <div className="mx-auto max-w-7xl px-4 sm:px-6 lg:px-8">
          <div className="relative overflow-hidden rounded-3xl bg-gradient-to-br from-primary-500/10 via-secondary-500/5 to-accent-500/10 border border-primary-500/10 p-10 sm:p-16 text-center">
            <div className="absolute top-0 left-1/2 -translate-x-1/2 w-[500px] h-[500px] bg-primary-500/5 rounded-full blur-3xl" />
            <div className="relative">
              <h2 className="text-3xl font-bold text-dark-50 sm:text-4xl">
                {"\uC9C0\uAE08 \uBC14\uB85C \uC2DC\uC791\uD558\uC138\uC694"}
              </h2>
              <p className="mt-4 text-dark-300 max-w-lg mx-auto text-lg">
                {"\uBCC4\uB3C4\uC758 \uC124\uCE58 \uC5C6\uC774 \uC6F9 \uBE0C\uB77C\uC6B0\uC800\uC5D0\uC11C \uBC14\uB85C ZeroClaw\uB97C \uCCB4\uD5D8\uD574\uBCF4\uC138\uC694."}
              </p>
              <p className="mt-2 text-dark-400 max-w-md mx-auto text-sm">
                {"\uB610\uB294 \uCE74\uCE74\uC624\uD1A1\uC73C\uB85C \uBC14\uB85C \uB300\uD654\uB97C \uC2DC\uC791\uD558\uC138\uC694. \uC571 \uC124\uCE58\uAC00 \uD544\uC694 \uC5C6\uC2B5\uB2C8\uB2E4."}
              </p>
              <div className="mt-8 flex flex-col items-center justify-center gap-4 sm:flex-row">
                <Link href="/chat" className="btn-primary px-8 py-3.5 text-base glow-primary">
                  {"\uC6F9\uC5D0\uC11C \uCC44\uD305 \uC2DC\uC791"}
                  <svg className="ml-2 h-4 w-4" fill="none" viewBox="0 0 24 24" strokeWidth={2} stroke="currentColor">
                    <path strokeLinecap="round" strokeLinejoin="round" d="M13.5 4.5L21 12m0 0l-7.5 7.5M21 12H3" />
                  </svg>
                </Link>
                <Link href="/download" className="btn-secondary px-8 py-3.5 text-base">
                  {"\uC571 \uB2E4\uC6B4\uB85C\uB4DC"}
                  <svg className="ml-2 h-4 w-4" fill="none" viewBox="0 0 24 24" strokeWidth={2} stroke="currentColor">
                    <path strokeLinecap="round" strokeLinejoin="round" d="M3 16.5v2.25A2.25 2.25 0 005.25 21h13.5A2.25 2.25 0 0021 18.75V16.5M16.5 12L12 16.5m0 0L7.5 12m4.5 4.5V3" />
                  </svg>
                </Link>
                <a
                  href="https://github.com/AiFlowTools/MoA"
                  target="_blank"
                  rel="noopener noreferrer"
                  className="btn-secondary px-8 py-3.5 text-base"
                >
                  GitHub
                  <svg className="ml-2 h-4 w-4" fill="none" viewBox="0 0 24 24" strokeWidth={2} stroke="currentColor">
                    <path strokeLinecap="round" strokeLinejoin="round" d="M13.5 6H5.25A2.25 2.25 0 003 8.25v10.5A2.25 2.25 0 005.25 21h10.5A2.25 2.25 0 0018 18.75V10.5m-10.5 6L21 3m0 0h-5.25M21 3v5.25" />
                  </svg>
                </a>
              </div>
            </div>
          </div>
        </div>
      </section>

      <Footer />
    </>
  );
}
