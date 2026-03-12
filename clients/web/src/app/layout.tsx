import type { Metadata, Viewport } from "next";
import "./globals.css";
import Header from "@/components/Header";

export const viewport: Viewport = {
  width: "device-width",
  initialScale: 1,
  themeColor: "#0f172a",
};

export const metadata: Metadata = {
  metadataBase: new URL("https://moa-web.vercel.app"),
  title: "MoA | \uD074\uB85C\uB4DC\uBD07\uC758 \uD55C\uACC4\uB97C \uB118\uC740 \uC790\uC728 AI \uC5D0\uC774\uC804\uD2B8",
  description:
    "MoA\uB294 ChaCha20 + AES-256 \uC774\uC911 \uC554\uD638\uD654, 23\uAC1C+ AI \uBAA8\uB378, 18\uAC1C+ \uCC44\uB110(\uCE74\uCE74\uC624\uD1A1 \uD3EC\uD568), 25\uAC1C+ \uB3C4\uAD6C\uB97C \uD0D1\uC7AC\uD55C \uCC28\uC138\uB300 AI \uC5D0\uC774\uC804\uD2B8\uC785\uB2C8\uB2E4. 3.4MB \uCD08\uACBD\uB7C9 \uBC14\uC774\uB108\uB9AC, \uC6D0\uD074\uB9AD \uC124\uCE58, \uC2E4\uC2DC\uAC04 \uB3D9\uAE30\uD654. MoA: next-gen AI agent with military-grade encryption, 23+ models, 18+ channels including KakaoTalk, and 25+ tools.",
  keywords: [
    "MoA",
    "AI agent",
    "\uCE74\uCE74\uC624\uD1A1 AI",
    "AI \uC5D0\uC774\uC804\uD2B8",
    "\uC790\uC728 AI",
    "\uBA40\uD2F0\uBAA8\uB378",
    "cross-platform",
    "\uD074\uB85C\uB4DC\uBD07 \uB300\uC548",
    "OpenClaw alternative",
    "AI \uBCF4\uC548",
    "ChaCha20 AES-256",
  ],
  authors: [{ name: "MoA Team" }],
  openGraph: {
    type: "website",
    locale: "ko_KR",
    alternateLocale: "en_US",
    url: "https://moa-agent.com",
    siteName: "MoA",
    title: "MoA | \uD074\uB85C\uB4DC\uBD07\uC758 \uD55C\uACC4\uB97C \uB118\uC740 AI \uC5D0\uC774\uC804\uD2B8",
    description:
      "ChaCha20 + AES-256 \uC774\uC911 \uC554\uD638\uD654, 23\uAC1C+ AI \uBAA8\uB378, 18\uAC1C+ \uCC44\uB110, 25\uAC1C+ \uB3C4\uAD6C. \uD074\uB85C\uB4DC\uBD07\uC758 \uD55C\uACC4\uB97C \uB118\uC740 \uCC28\uC138\uB300 AI \uC5D0\uC774\uC804\uD2B8.",
    images: [
      {
        url: "/og-image.png",
        width: 1200,
        height: 630,
        alt: "MoA",
      },
    ],
  },
  twitter: {
    card: "summary_large_image",
    title: "MoA",
    description:
      "\uD074\uB85C\uB4DC\uBD07\uC758 \uD55C\uACC4\uB97C \uB118\uC740 \uCC28\uC138\uB300 AI \uC5D0\uC774\uC804\uD2B8. 23+ \uBAA8\uB378, 18+ \uCC44\uB110, \uCE74\uCE74\uC624\uD1A1 \uC5F0\uB3D9.",
    images: ["/og-image.png"],
  },
  robots: {
    index: true,
    follow: true,
  },
};

export default function RootLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <html lang="ko" className="dark">
      <body
        className="font-sans bg-dark-950 text-dark-100 antialiased"
      >
        <Header />
        <main className="min-h-screen">{children}</main>
      </body>
    </html>
  );
}
