import type { NextConfig } from "next";
import createNextIntlPlugin from "next-intl/plugin";

const withNextIntl = createNextIntlPlugin("./src/i18n/request.ts");

// Rust API origin — rewrites keep the browser same-origin so session/CSRF
// cookies flow without CORS (spec §4.10).
const API_ORIGIN = process.env.API_ORIGIN ?? "http://127.0.0.1:8080";

const nextConfig: NextConfig = {
  poweredByHeader: false,
  // Public booking/invitation capabilities must not appear in dev access logs.
  logging: { incomingRequests: false },
  allowedDevOrigins: ["127.0.0.1", "localhost"],
  reactStrictMode: true,
  async rewrites() {
    return [
      { source: "/api/v1/:path*", destination: `${API_ORIGIN}/api/v1/:path*` },
      {
        source: "/public/v1/:path*",
        destination: `${API_ORIGIN}/public/v1/:path*`,
      },
      {
        source: "/webhooks/v1/:path*",
        destination: `${API_ORIGIN}/webhooks/v1/:path*`,
      },
    ];
  },
  async headers() {
    return [
      {
        source: "/:path*",
        headers: [
          { key: "X-Content-Type-Options", value: "nosniff" },
          { key: "X-Frame-Options", value: "DENY" },
          { key: "Referrer-Policy", value: "strict-origin-when-cross-origin" },
          {
            key: "Permissions-Policy",
            value: "camera=(), microphone=(), geolocation=()",
          },
        ],
      },
      {
        // Public token pages: no indexing, no referrer leakage, no caching
        // (source §7.6).
        source: "/public/:path*",
        headers: [
          { key: "X-Robots-Tag", value: "noindex" },
          { key: "Referrer-Policy", value: "no-referrer" },
          { key: "Cache-Control", value: "no-store" },
        ],
      },
    ];
  },
};

export default withNextIntl(nextConfig);
