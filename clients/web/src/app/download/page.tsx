"use client";

import { useState, useEffect } from "react";
import Footer from "@/components/Footer";

type Platform = "windows" | "macos" | "linux" | "android" | "ios" | "unknown";

function detectPlatform(): Platform {
  if (typeof window === "undefined") return "unknown";
  const ua = navigator.userAgent.toLowerCase();
  if (ua.includes("win")) return "windows";
  if (ua.includes("mac")) return "macos";
  if (ua.includes("linux") && !ua.includes("android")) return "linux";
  if (ua.includes("android")) return "android";
  if (ua.includes("iphone") || ua.includes("ipad") || ua.includes("ipod"))
    return "ios";
  return "unknown";
}

const R2_BASE =
  process.env.NEXT_PUBLIC_R2_BASE_URL || "https://downloads.moa-agent.com";

interface DownloadItem {
  platform: Platform;
  icon: string;
  name: string;
  nameKo: string;
  variants: {
    label: string;
    filename: string;
    url: string;
    badge?: string;
  }[];
  requirements: string[];
  requirementsKo: string[];
  instructions: string[];
  instructionsKo: string[];
}

const downloads: DownloadItem[] = [
  {
    platform: "windows",
    icon: "\uD83E\uDE9F",
    name: "Windows",
    nameKo: "\uC708\uB3C4\uC6B0",
    variants: [
      {
        label: "Windows x64 Installer (.msi)",
        filename: "MoA-x64-setup.msi",
        url: `${R2_BASE}/releases/latest/MoA-x64-setup.msi`,
        badge: "Recommended",
      },
      {
        label: "Windows x64 Portable (.exe)",
        filename: "MoA-x64-portable.exe",
        url: `${R2_BASE}/releases/latest/MoA-x64-portable.exe`,
      },
    ],
    requirements: [
      "Windows 10 or later (64-bit)",
      "4GB RAM minimum",
      "200MB disk space",
      "WebView2 Runtime (included in Windows 10/11)",
    ],
    requirementsKo: [
      "Windows 10 \uC774\uC0C1 (64\uBE44\uD2B8)",
      "\uCD5C\uC18C 4GB RAM",
      "200MB \uB514\uC2A4\uD06C \uACF5\uAC04",
      "WebView2 \uB7F0\uD0C0\uC784 (Windows 10/11 \uD3EC\uD568)",
    ],
    instructions: [
      "Download the .msi installer",
      "Double-click the downloaded file",
      "Follow the installation wizard",
      "Launch MoA from Start Menu",
    ],
    instructionsKo: [
      ".msi \uC124\uCE58 \uD30C\uC77C \uB2E4\uC6B4\uB85C\uB4DC",
      "\uB2E4\uC6B4\uB85C\uB4DC\uB41C \uD30C\uC77C \uB354\uBE14 \uD074\uB9AD",
      "\uC124\uCE58 \uB9C8\uBC95\uC0AC \uC548\uB0B4 \uB530\uB77C \uC124\uCE58",
      "\uC2DC\uC791 \uBA54\uB274\uC5D0\uC11C MoA \uC2E4\uD589",
    ],
  },
  {
    platform: "macos",
    icon: "\uD83C\uDF4E",
    name: "macOS",
    nameKo: "\uB9E5OS",
    variants: [
      {
        label: "macOS Universal (.dmg)",
        filename: "MoA-universal.dmg",
        url: `${R2_BASE}/releases/latest/MoA-universal.dmg`,
        badge: "Intel + Apple Silicon",
      },
      {
        label: "macOS Apple Silicon (.dmg)",
        filename: "MoA-aarch64.dmg",
        url: `${R2_BASE}/releases/latest/MoA-aarch64.dmg`,
      },
      {
        label: "macOS Intel (.dmg)",
        filename: "MoA-x64.dmg",
        url: `${R2_BASE}/releases/latest/MoA-x64.dmg`,
      },
    ],
    requirements: [
      "macOS 11 (Big Sur) or later",
      "Apple Silicon (M1/M2/M3) or Intel",
      "4GB RAM minimum",
      "200MB disk space",
    ],
    requirementsKo: [
      "macOS 11 (Big Sur) \uC774\uC0C1",
      "Apple Silicon (M1/M2/M3) \uB610\uB294 Intel",
      "\uCD5C\uC18C 4GB RAM",
      "200MB \uB514\uC2A4\uD06C \uACF5\uAC04",
    ],
    instructions: [
      "Download the .dmg file",
      "Open the .dmg file",
      "Drag MoA to Applications folder",
      "Open MoA from Applications or Launchpad",
      "If blocked: System Settings > Privacy & Security > Allow",
    ],
    instructionsKo: [
      ".dmg \uD30C\uC77C \uB2E4\uC6B4\uB85C\uB4DC",
      ".dmg \uD30C\uC77C \uC5F4\uAE30",
      "MoA\uB97C \uC751\uC6A9 \uD504\uB85C\uADF8\uB7A8 \uD3F4\uB354\uB85C \uB4DC\uB798\uADF8",
      "\uC751\uC6A9 \uD504\uB85C\uADF8\uB7A8 \uB610\uB294 Launchpad\uC5D0\uC11C MoA \uC2E4\uD589",
      "\uCC28\uB2E8 \uC2DC: \uC2DC\uC2A4\uD15C \uC124\uC815 > \uAC1C\uC778\uC815\uBCF4 \uBCF4\uD638 > \uD5C8\uC6A9",
    ],
  },
  {
    platform: "linux",
    icon: "\uD83D\uDC27",
    name: "Linux",
    nameKo: "\uB9AC\uB205\uC2A4",
    variants: [
      {
        label: "Linux x64 AppImage",
        filename: "MoA-x64.AppImage",
        url: `${R2_BASE}/releases/latest/MoA-x64.AppImage`,
        badge: "Universal",
      },
      {
        label: "Linux x64 Debian (.deb)",
        filename: "MoA-x64.deb",
        url: `${R2_BASE}/releases/latest/MoA-x64.deb`,
      },
      {
        label: "Linux x64 RPM (.rpm)",
        filename: "MoA-x64.rpm",
        url: `${R2_BASE}/releases/latest/MoA-x64.rpm`,
      },
    ],
    requirements: [
      "Ubuntu 20.04+ / Fedora 36+ / Arch Linux or equivalent",
      "x86_64 architecture",
      "4GB RAM minimum",
      "200MB disk space",
      "WebKitGTK 4.1+",
    ],
    requirementsKo: [
      "Ubuntu 20.04+ / Fedora 36+ / Arch Linux \uB610\uB294 \uD638\uD658 \uBC30\uD3EC\uD310",
      "x86_64 \uC544\uD0A4\uD14D\uCC98",
      "\uCD5C\uC18C 4GB RAM",
      "200MB \uB514\uC2A4\uD06C \uACF5\uAC04",
      "WebKitGTK 4.1+",
    ],
    instructions: [
      "AppImage: chmod +x MoA-x64.AppImage && ./MoA-x64.AppImage",
      "Debian/Ubuntu: sudo dpkg -i MoA-x64.deb",
      "Fedora/RHEL: sudo rpm -i MoA-x64.rpm",
    ],
    instructionsKo: [
      "AppImage: chmod +x MoA-x64.AppImage && ./MoA-x64.AppImage",
      "Debian/Ubuntu: sudo dpkg -i MoA-x64.deb",
      "Fedora/RHEL: sudo rpm -i MoA-x64.rpm",
    ],
  },
  {
    platform: "android",
    icon: "\uD83E\uDD16",
    name: "Android",
    nameKo: "\uC548\uB4DC\uB85C\uC774\uB4DC",
    variants: [
      {
        label: "Android APK (Direct Download)",
        filename: "MoA-arm64.apk",
        url: `${R2_BASE}/releases/latest/MoA-arm64.apk`,
        badge: "APK",
      },
      {
        label: "Google Play Store",
        filename: "",
        url: "#",
        badge: "Coming Soon",
      },
    ],
    requirements: [
      "Android 8.0 (Oreo) or later",
      "ARM64 architecture",
      "2GB RAM minimum",
      "100MB storage",
    ],
    requirementsKo: [
      "Android 8.0 (Oreo) \uC774\uC0C1",
      "ARM64 \uC544\uD0A4\uD14D\uCC98",
      "\uCD5C\uC18C 2GB RAM",
      "100MB \uC800\uC7A5 \uACF5\uAC04",
    ],
    instructions: [
      "Download the .apk file",
      "Enable 'Install from Unknown Sources' in Settings",
      "Open the .apk file to install",
      "Launch MoA from app drawer",
    ],
    instructionsKo: [
      ".apk \uD30C\uC77C \uB2E4\uC6B4\uB85C\uB4DC",
      "\uC124\uC815\uC5D0\uC11C '\uCD9C\uCC98\uB97C \uC54C \uC218 \uC5C6\uB294 \uC571 \uC124\uCE58' \uD5C8\uC6A9",
      ".apk \uD30C\uC77C\uC744 \uC5F4\uC5B4 \uC124\uCE58",
      "\uC571 \uC11C\uB78D\uC5D0\uC11C MoA \uC2E4\uD589",
    ],
  },
  {
    platform: "ios",
    icon: "\uD83D\uDCF1",
    name: "iOS",
    nameKo: "iOS",
    variants: [
      {
        label: "App Store",
        filename: "",
        url: "#",
        badge: "Coming Soon",
      },
      {
        label: "TestFlight Beta",
        filename: "",
        url: "#",
        badge: "Beta",
      },
    ],
    requirements: [
      "iOS 16.0 or later",
      "iPhone 8 or later",
      "100MB storage",
    ],
    requirementsKo: [
      "iOS 16.0 \uC774\uC0C1",
      "iPhone 8 \uC774\uC0C1",
      "100MB \uC800\uC7A5 \uACF5\uAC04",
    ],
    instructions: [
      "Open App Store on your device",
      "Search for 'MoA'",
      "Tap 'Get' to download and install",
      "Open MoA from home screen",
    ],
    instructionsKo: [
      "\uAE30\uAE30\uC5D0\uC11C App Store \uC5F4\uAE30",
      "'MoA' \uAC80\uC0C9",
      "'\uBC1B\uAE30' \uD0ED\uD558\uC5EC \uB2E4\uC6B4\uB85C\uB4DC \uBC0F \uC124\uCE58",
      "\uD648 \uD654\uBA74\uC5D0\uC11C MoA \uC2E4\uD589",
    ],
  },
];

export default function DownloadPage() {
  const [currentPlatform, setCurrentPlatform] = useState<Platform>("unknown");
  const [expandedPlatform, setExpandedPlatform] = useState<Platform | null>(
    null,
  );

  useEffect(() => {
    const detected = detectPlatform();
    setCurrentPlatform(detected);
    setExpandedPlatform(detected !== "unknown" ? detected : null);
  }, []);

  const togglePlatform = (platform: Platform) => {
    setExpandedPlatform(expandedPlatform === platform ? null : platform);
  };

  return (
    <>
      <div className="pt-32 pb-20">
        <div className="mx-auto max-w-5xl px-4 sm:px-6 lg:px-8">
          {/* Header */}
          <div className="text-center mb-16">
            <h1 className="text-3xl font-bold text-dark-50 sm:text-4xl lg:text-5xl">
              {"\uB2E4\uC6B4\uB85C\uB4DC"}
              <span className="text-dark-500 ml-3 text-xl font-normal sm:text-2xl">
                Download
              </span>
            </h1>
            <p className="mt-4 text-dark-400 max-w-xl mx-auto">
              {"\uBAA8\uB4E0 \uD50C\uB7AB\uD3FC\uC5D0\uC11C MoA\uB97C \uC0AC\uC6A9\uD558\uC138\uC694. \uC6F9 \uBE0C\uB77C\uC6B0\uC800\uC5D0\uC11C\uB3C4 \uBC14\uB85C \uC0AC\uC6A9 \uAC00\uB2A5\uD569\uB2C8\uB2E4."}
            </p>
            <p className="mt-1 text-sm text-dark-500">
              Use MoA on every platform. Also available directly in your browser.
            </p>

            {currentPlatform !== "unknown" && (
              <div className="mt-6 inline-flex items-center gap-2 rounded-full border border-accent-500/20 bg-accent-500/5 px-4 py-1.5">
                <div className="h-1.5 w-1.5 rounded-full bg-accent-400" />
                <span className="text-xs font-medium text-accent-300">
                  {"\uAC10\uC9C0\uB41C OS: "}
                  {downloads.find((d) => d.platform === currentPlatform)?.name ||
                    "Unknown"}
                </span>
              </div>
            )}
          </div>

          {/* Download cards */}
          <div className="space-y-4">
            {downloads.map((item) => {
              const isExpanded = expandedPlatform === item.platform;
              const isCurrent = currentPlatform === item.platform;

              return (
                <div
                  key={item.platform}
                  className={`rounded-2xl border transition-all duration-300 ${
                    isCurrent
                      ? "border-primary-500/30 bg-primary-500/5"
                      : "border-dark-800/50 bg-dark-900/50"
                  }`}
                >
                  {/* Card header */}
                  <button
                    onClick={() => togglePlatform(item.platform)}
                    className="flex w-full items-center justify-between px-6 py-5 text-left"
                  >
                    <div className="flex items-center gap-4">
                      <span className="text-3xl">{item.icon}</span>
                      <div>
                        <h2 className="text-lg font-semibold text-dark-50 flex items-center gap-2">
                          {item.nameKo}{" "}
                          <span className="text-dark-500 text-sm font-normal">
                            {item.name}
                          </span>
                          {isCurrent && (
                            <span className="rounded-full bg-primary-500/20 px-2 py-0.5 text-[10px] font-medium text-primary-400">
                              {"\uD604\uC7AC OS"} Current
                            </span>
                          )}
                        </h2>
                        <p className="text-xs text-dark-500 mt-0.5">
                          {item.variants.length}{" "}
                          {item.variants.length === 1
                            ? "download option"
                            : "download options"}{" "}
                          available
                        </p>
                      </div>
                    </div>
                    <svg
                      className={`h-5 w-5 text-dark-400 transition-transform duration-200 ${
                        isExpanded ? "rotate-180" : ""
                      }`}
                      fill="none"
                      viewBox="0 0 24 24"
                      strokeWidth={2}
                      stroke="currentColor"
                    >
                      <path
                        strokeLinecap="round"
                        strokeLinejoin="round"
                        d="M19.5 8.25l-7.5 7.5-7.5-7.5"
                      />
                    </svg>
                  </button>

                  {/* Expanded content */}
                  {isExpanded && (
                    <div className="border-t border-dark-800/50 px-6 py-6 space-y-6 animate-fade-in">
                      {/* Download buttons */}
                      <div>
                        <h3 className="text-sm font-semibold text-dark-200 mb-3">
                          {"\uB2E4\uC6B4\uB85C\uB4DC"} Downloads
                        </h3>
                        <div className="grid gap-3 sm:grid-cols-2">
                          {item.variants.map((variant) => {
                            const isDisabled = variant.url === "#";
                            return (
                              <a
                                key={variant.label}
                                href={isDisabled ? undefined : variant.url}
                                className={`flex items-center justify-between rounded-xl border px-4 py-3 transition-all ${
                                  isDisabled
                                    ? "border-dark-800 bg-dark-900/50 cursor-not-allowed opacity-60"
                                    : "border-dark-700 bg-dark-800/50 hover:border-primary-500/30 hover:bg-primary-500/5"
                                }`}
                                {...(isDisabled
                                  ? {}
                                  : { download: variant.filename })}
                                onClick={
                                  isDisabled
                                    ? (e: React.MouseEvent) =>
                                        e.preventDefault()
                                    : undefined
                                }
                              >
                                <div>
                                  <div className="text-sm font-medium text-dark-200">
                                    {variant.label}
                                  </div>
                                  {variant.filename && (
                                    <div className="text-[10px] text-dark-500 font-mono mt-0.5">
                                      {variant.filename}
                                    </div>
                                  )}
                                </div>
                                {variant.badge && (
                                  <span
                                    className={`rounded-full px-2 py-0.5 text-[10px] font-medium ${
                                      variant.badge === "Coming Soon"
                                        ? "bg-dark-700 text-dark-400"
                                        : variant.badge === "Recommended"
                                          ? "bg-primary-500/20 text-primary-400"
                                          : "bg-accent-500/20 text-accent-400"
                                    }`}
                                  >
                                    {variant.badge}
                                  </span>
                                )}
                              </a>
                            );
                          })}
                        </div>
                      </div>

                      <div className="grid gap-6 sm:grid-cols-2">
                        {/* System requirements */}
                        <div>
                          <h3 className="text-sm font-semibold text-dark-200 mb-3">
                            {"\uC2DC\uC2A4\uD15C \uC694\uAD6C \uC0AC\uD56D"} System Requirements
                          </h3>
                          <ul className="space-y-1.5">
                            {item.requirementsKo.map((req, i) => (
                              <li
                                key={i}
                                className="flex items-start gap-2 text-xs"
                              >
                                <span className="text-dark-600 mt-0.5">
                                  &bull;
                                </span>
                                <span className="text-dark-400">{req}</span>
                              </li>
                            ))}
                          </ul>
                        </div>

                        {/* Installation */}
                        <div>
                          <h3 className="text-sm font-semibold text-dark-200 mb-3">
                            {"\uC124\uCE58 \uBC29\uBC95"} Installation
                          </h3>
                          <ol className="space-y-1.5">
                            {item.instructionsKo.map((step, i) => (
                              <li
                                key={i}
                                className="flex items-start gap-2 text-xs"
                              >
                                <span className="flex h-4 w-4 flex-shrink-0 items-center justify-center rounded-full bg-dark-800 text-[10px] text-dark-400 font-mono">
                                  {i + 1}
                                </span>
                                <span className="text-dark-400">{step}</span>
                              </li>
                            ))}
                          </ol>
                        </div>
                      </div>
                    </div>
                  )}
                </div>
              );
            })}
          </div>

          {/* Web version CTA */}
          <div className="mt-12 text-center">
            <div className="glass-card inline-block rounded-2xl px-8 py-6">
              <h3 className="text-lg font-semibold text-dark-50">
                {"\uC124\uCE58 \uC5C6\uC774 \uC0AC\uC6A9\uD558\uAE30"}{" "}
                <span className="text-dark-500 text-sm font-normal">
                  Use without installation
                </span>
              </h3>
              <p className="mt-2 text-sm text-dark-400">
                {"\uC6F9 \uBE0C\uB77C\uC6B0\uC800\uC5D0\uC11C \uBC14\uB85C MoA\uB97C \uC0AC\uC6A9\uD560 \uC218 \uC788\uC2B5\uB2C8\uB2E4."}
              </p>
              <a href="/chat" className="btn-accent mt-4 inline-flex px-6 py-2.5">
                {"\uC6F9\uC5D0\uC11C \uCC44\uD305 \uC2DC\uC791"} Start Web Chat
                <svg
                  className="ml-2 h-4 w-4"
                  fill="none"
                  viewBox="0 0 24 24"
                  strokeWidth={2}
                  stroke="currentColor"
                >
                  <path
                    strokeLinecap="round"
                    strokeLinejoin="round"
                    d="M13.5 4.5L21 12m0 0l-7.5 7.5M21 12H3"
                  />
                </svg>
              </a>
            </div>
          </div>
        </div>
      </div>
      <Footer />
    </>
  );
}
