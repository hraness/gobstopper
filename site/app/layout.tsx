import type { Metadata, Viewport } from "next";

import { FoilController } from "./foil-controller";

import {
  GITHUB_URL,
  SITE_DESCRIPTION,
  SITE_NAME,
  SITE_ORIGIN,
  absoluteUrl,
  serializeJsonLd,
} from "./_lib/site";
import "@hraness/design-kit/fonts.css";
import "./globals.css";

const title = `${SITE_NAME} — earlier, smarter context compaction for coding agents`;
const description = SITE_DESCRIPTION;
const websiteId = `${absoluteUrl("/")}#website`;
const applicationId = `${absoluteUrl("/")}#application`;

export const metadata: Metadata = {
  metadataBase: new URL(SITE_ORIGIN),
  title: {
    default: title,
    template: `%s | ${SITE_NAME}`,
  },
  description,
  applicationName: SITE_NAME,
  authors: [{ name: SITE_NAME, url: SITE_ORIGIN }],
  creator: SITE_NAME,
  publisher: SITE_NAME,
  category: "developer tools",
  keywords: [
    "Gobstopper",
    "context compaction",
    "Codex",
    "Claude Code",
    "coding agent",
    "context window",
    "token savings",
  ],
  manifest: "/manifest.webmanifest",
  alternates: { canonical: "/" },
  icons: {
    apple: [{ url: "/apple-icon.png", sizes: "180x180", type: "image/png" }],
    icon: [{ type: "image/png", url: "/icon.png", sizes: "512x512" }],
  },
  robots: {
    index: true,
    follow: true,
    googleBot: {
      index: true,
      follow: true,
      "max-image-preview": "large",
      "max-snippet": -1,
      "max-video-preview": -1,
    },
  },
  openGraph: {
    type: "website",
    siteName: SITE_NAME,
    title,
    description,
    url: "/",
    images: [
      {
        url: "/opengraph-image",
        width: 1200,
        height: 630,
        alt: title,
      },
    ],
  },
  twitter: {
    card: "summary_large_image",
    title,
    description,
    images: [{ url: "/opengraph-image", alt: title }],
  },
};

export const viewport: Viewport = {
  colorScheme: "light dark",
  themeColor: [
    { color: "#f8f7f4", media: "(prefers-color-scheme: light)" },
    { color: "#12100f", media: "(prefers-color-scheme: dark)" },
  ],
};

const structuredData = {
  "@context": "https://schema.org",
  "@graph": [
    {
      "@type": "WebSite",
      "@id": websiteId,
      name: SITE_NAME,
      url: absoluteUrl("/"),
      description,
      inLanguage: "en-US",
    },
    {
      "@type": "SoftwareApplication",
      "@id": applicationId,
      name: SITE_NAME,
      url: absoluteUrl("/"),
      description,
      applicationCategory: "DeveloperApplication",
      operatingSystem: "macOS, Linux",
      sameAs: GITHUB_URL,
      featureList: [
        "Codex and Claude Code transcript detection",
        "Policy-driven context compaction",
        "Vault snapshots and undo",
        "Provider-native and file-fork strategies",
        "Pluggable bounded strategy plugins",
      ],
      isPartOf: { "@id": websiteId },
    },
    {
      "@type": "SoftwareSourceCode",
      name: SITE_NAME,
      description,
      codeRepository: GITHUB_URL,
      programmingLanguage: "Rust",
      runtimePlatform: "Cargo",
      license: "https://opensource.org/license/mit",
      targetProduct: { "@id": applicationId },
    },
  ],
};

export default function RootLayout({
  children,
}: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en" data-hraness-theme="paper" data-hraness-material="lantern">
      <body>
        <FoilController />
        <script
          dangerouslySetInnerHTML={{ __html: serializeJsonLd(structuredData) }}
          type="application/ld+json"
        />
        {children}
      </body>
    </html>
  );
}
