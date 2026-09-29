import { fileURLToPath } from "node:url";
import type { NextConfig } from "next";

// `curl … | sh` and `irm … | iex` read the installers as text.
const installerHeaders = [
  { key: "Content-Type", value: "text/plain; charset=utf-8" },
  { key: "Cache-Control", value: "public, max-age=300, must-revalidate" },
];

const nextConfig: NextConfig = {
  outputFileTracingRoot: fileURLToPath(new URL(".", import.meta.url)),
  async headers() {
    return [
      { source: "/install.sh", headers: installerHeaders },
      { source: "/install.ps1", headers: installerHeaders },
    ];
  },
};

export default nextConfig;
