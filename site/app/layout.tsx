import type { Metadata, Viewport } from "next";
import { HranessSiteFooter } from "@hraness/site-footer/react";
import { siteDescription, siteTitle } from "./site";
import { supportProfile } from "./support-profile";
import "./globals.css";

const title = siteTitle;
const description = siteDescription;

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
    images: [{ url: "/opengraph-image", width: 1200, height: 630, alt: title }],
  },
  twitter: {
    card: "summary_large_image",
    title,
    description,
    images: [{ url: "/opengraph-image", alt: title }],
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
