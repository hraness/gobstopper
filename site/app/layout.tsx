import type { Metadata, Viewport } from "next";
import { getDesignPaletteTheme } from "@hraness/design-kit";
import { DesignPaletteProvider, ThemeColorSync } from "@hraness/design-kit/react";
import { siteDefaultPalette } from "../palette";

import { FoilController } from "./foil-controller";

import {
  GITHUB_URL,
  SITE_DESCRIPTION,
  SITE_NAME,
  SITE_ORIGIN,
  SITE_TITLE,
  absoluteUrl,
  serializeJsonLd,
} from "./_lib/site";
import "@hraness/design-kit/fonts.css";
import "./globals.css";

const initialPalette = getDesignPaletteTheme("tokyo-night", "light");

const title = SITE_TITLE;
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
    { color: "#e1e2e7", media: "(prefers-color-scheme: light)" },
    { color: "#1a1b26", media: "(prefers-color-scheme: dark)" },
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
        "Claude Code and Codex session detection",
        "Compaction previews at a trigger and floor you choose",
        "Compacted Claude Code and Codex copies",
        "Local snapshot vault with search and undo",
        "Plugins for your own strategies",
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
      license: [
        "https://opensource.org/license/mit",
        "https://opensource.org/license/apache-2-0",
      ],
      targetProduct: { "@id": applicationId },
    },
  ],
};

export default function RootLayout({
  children,
}: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en" data-hraness-theme="paper" data-hraness-material="lantern" data-hraness-pattern="mesh" data-palette="tokyo-night" className={initialPalette.className} suppressHydrationWarning>
      <head>
        {/* eslint-disable-next-line @next/next/no-sync-scripts */}
        <script src="/theme-bootstrap.js" />
      </head>
      <body>
        <DesignPaletteProvider defaultPreference={siteDefaultPalette}>
        <ThemeColorSync />
        <FoilController />
        <script
          dangerouslySetInnerHTML={{ __html: serializeJsonLd(structuredData) }}
          type="application/ld+json"
        />
        {children}
        </DesignPaletteProvider>
      </body>
    </html>
  );
}
