import {
  createSocialImageResponse,
  socialImageContentType as contentType,
  socialImageSize as size,
} from "@hraness/web-discovery/social-image";

export const alt = "Gobstopper — Compact earlier. Spend less. Keep the thread.";
export { contentType, size };

function GobstopperMark() {
  return (
    <svg aria-label="Gobstopper mark" height="42" role="img" viewBox="0 0 42 42" width="42">
      <circle cx="21" cy="21" fill="none" r="17" stroke="currentColor" strokeWidth="3" />
      <circle cx="21" cy="21" fill="none" r="10" stroke="currentColor" strokeWidth="3" />
      <circle cx="21" cy="21" fill="currentColor" r="3" />
    </svg>
  );
}

export default function OpengraphImage() {
  return createSocialImageResponse({
    description: "Automatic context compaction for Codex, Claude Code, and Devin sessions.",
    domain: "gobstopper.sh",
    eyebrow: "Gobstopper",
    mark: <GobstopperMark />,
    theme: {
      accent: "#A34733",
      background: "#F8F7F4",
      foreground: "#1C1A18",
      muted: "#6A655E",
    },
    title: "Compact earlier. Spend less. Keep the thread.",
  });
}
