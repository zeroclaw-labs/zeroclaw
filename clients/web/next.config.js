/** @type {import('next').NextConfig} */
const nextConfig = {
  env: {
    NEXT_PUBLIC_DEFAULT_SERVER_URL:
      process.env.NEXT_PUBLIC_DEFAULT_SERVER_URL || "http://localhost:8080",
    NEXT_PUBLIC_R2_BASE_URL:
      process.env.NEXT_PUBLIC_R2_BASE_URL ||
      "https://downloads.moa-agent.com",
    NEXT_PUBLIC_KAKAO_REST_API_KEY:
      process.env.NEXT_PUBLIC_KAKAO_REST_API_KEY || "",
  },
  async headers() {
    return [
      {
        source: "/:path*",
        headers: [
          { key: "X-Frame-Options", value: "DENY" },
          { key: "X-Content-Type-Options", value: "nosniff" },
          { key: "Referrer-Policy", value: "strict-origin-when-cross-origin" },
        ],
      },
    ];
  },
};

module.exports = nextConfig;
