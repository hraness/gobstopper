import { fileURLToPath } from "node:url";
import type { NextConfig } from "next";

// `curl … | sh` and `irm … | iex` read the installers as text.
const installerHeaders = [
  { key: "Content-Type", value: "text/plain; charset=utf-8" },
  { key: "Cache-Control", value: "public, max-age=300, must-revalidate" },
];

// Next.js emits inline bootstrap scripts, so script-src keeps 'unsafe-inline'.
const contentSecurityPolicy = [
  "default-src 'self'",
  "script-src 'self' 'unsafe-inline' https://*.posthog.com",
  "style-src 'self' 'unsafe-inline'",
  "img-src 'self' data: blob: https://raw.githubusercontent.com",
  "font-src 'self' data:",
  "connect-src 'self' https://*.posthog.com https://account.hraness.com",
  "media-src 'self'",
  "object-src 'none'",
  "base-uri 'self'",
  "form-action 'self'",
  "frame-ancestors 'none'",
  "upgrade-insecure-requests",
].join("; ");

const securityHeaders = [
  { key: "Content-Security-Policy", value: contentSecurityPolicy },
  { key: "X-Content-Type-Options", value: "nosniff" },
  { key: "Referrer-Policy", value: "strict-origin-when-cross-origin" },
  { key: "X-Frame-Options", value: "DENY" },
  {
    key: "Permissions-Policy",
    value: "camera=(), microphone=(), geolocation=(), payment=(), usb=()",
  },
  {
    key: "Strict-Transport-Security",
    value: "max-age=63072000; includeSubDomains",
  },
];

const nextConfig: NextConfig = {
  outputFileTracingRoot: fileURLToPath(new URL(".", import.meta.url)),
  async headers() {
    return [
      { source: "/:path*", headers: securityHeaders },
      { source: "/install.sh", headers: installerHeaders },
      { source: "/install.ps1", headers: installerHeaders },
    ];
  },
};

export default nextConfig;
