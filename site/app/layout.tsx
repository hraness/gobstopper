import type { Metadata, Viewport } from "next";
import { HranessSiteFooter } from "@hraness/site-footer/react";
import { supportProfile } from "./support-profile";
import "./globals.css";

const title = "Gobstopper: earlier, smarter context compaction for coding agents";
const description =
  "Gobstopper watches Codex and Claude Code sessions and compacts context at a threshold you control — so long sessions cost a fraction of the tokens and stay sharp.";

export const metadata: Metadata = {
  metadataBase: new URL("https://gobstopper.sh"),
  title,
  description,
  alternates: { canonical: "/" },
  icons: {
    apple: [{ url: "/apple-icon.png", sizes: "180x180", type: "image/png" }],
    icon: [{ type: "image/png", url: "/icon.png", sizes: "512x512" }],
  },
  openGraph: {
    title,
    description,
    siteName: "Gobstopper",
    type: "website",
    url: "/",
  },
  twitter: {
    card: "summary",
    title,
    description,
  },
};

export const viewport: Viewport = {
  themeColor: [
    { color: "#f8f7f4", media: "(prefers-color-scheme: light)" },
    { color: "#12100f", media: "(prefers-color-scheme: dark)" },
  ],
};

export default function RootLayout({
  children,
}: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en" data-hraness-theme="paper" data-hraness-material="lantern">
      <body>
        {children}
        <div className="network-footer">
          <HranessSiteFooter placement="flow" mailingList={{ kind: "none" }} support={supportProfile} />
        </div>
      </body>
    </html>
  );
}
