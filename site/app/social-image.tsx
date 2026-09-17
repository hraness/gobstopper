import { nebulaSansSocialFonts } from "@hraness/design-kit/fonts/nebula-sans/social";
import { ImageResponse } from "next/og";

export const socialImageSize = { height: 630, width: 1200 } as const;
export const socialImageContentType = "image/png";

const theme = {
  accent: "#2457A6",
  background: "#FFFFFF",
  foreground: "#171717",
  muted: "#666666",
} as const;

/**
 * Deterministic title-and-description social card: embedded Nebula Sans, inline
 * layout, no remote assets or filesystem reads so prerendering stays portable.
 */
export function createSocialImage(details: Readonly<{
  description: string;
  domain: string;
  eyebrow?: string;
  title: string;
}>): ImageResponse {
  return new ImageResponse(
    (
      <div
        style={{
          background: theme.background,
          color: theme.foreground,
          display: "flex",
          flexDirection: "column",
          fontFamily: "Nebula Sans",
          height: "100%",
          justifyContent: "space-between",
          padding: "58px 66px",
          width: "100%",
        }}
      >
        <div
          style={{
            alignItems: "center",
            borderBottom: `1px solid ${theme.muted}`,
            display: "flex",
            fontSize: 22,
            fontWeight: 700,
            justifyContent: "space-between",
            paddingBottom: 18,
          }}
        >
          <span>{details.eyebrow ?? details.domain}</span>
        </div>
        <div style={{ display: "flex", flexDirection: "column", gap: 28 }}>
          <div
            style={{
              fontSize: details.title.length > 58 ? 52 : 64,
              fontWeight: 700,
              letterSpacing: "-0.025em",
              lineHeight: 1.08,
              maxWidth: 1040,
            }}
          >
            {details.title}
          </div>
          <div
            style={{
              color: theme.muted,
              fontSize: 26,
              lineHeight: 1.4,
              maxWidth: 980,
            }}
          >
            {details.description}
          </div>
        </div>
        <div
          style={{
            alignItems: "center",
            borderTop: `1px solid ${theme.muted}`,
            display: "flex",
            fontSize: 21,
            justifyContent: "space-between",
          }}
        >
          <span style={{ color: theme.accent, paddingTop: 18 }}>{details.domain}</span>
        </div>
      </div>
    ),
    { ...socialImageSize, fonts: [...nebulaSansSocialFonts()] },
  );
}
