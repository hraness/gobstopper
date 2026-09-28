import type { ReactNode } from "react";

const INLINE_MARKUP = /`([^`]+)`|\[([^\]]+)\]\((\/[^)\s]*)\)/gu;

/** Renders `code` spans and site-relative [text](/path) links inside answer text. */
export function renderInline(text: string) {
  const nodes: ReactNode[] = [];
  let last = 0;
  for (const match of text.matchAll(INLINE_MARKUP)) {
    const index = match.index;
    if (index > last) nodes.push(text.slice(last, index));
    const [, code, label, href] = match;
    nodes.push(code === undefined ? <a href={href} key={index}>{label}</a> : <code key={index}>{code}</code>);
    last = index + match[0].length;
  }
  if (last < text.length) nodes.push(text.slice(last));
  return nodes;
}

/** Plain text for structured data: code marks dropped, links reduced to their anchor text. */
export function plainInline(text: string): string {
  return text.replace(INLINE_MARKUP, (_, code: string | undefined, label: string | undefined) => code ?? label ?? "");
}
