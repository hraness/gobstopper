import type { ReactNode } from "react";

export interface SiteDocumentTocItem {
  readonly href: `#${string}`;
  readonly label: string;
}

interface SiteDocumentProps {
  readonly children: ReactNode;
  /** One concrete claim, as a complete sentence. */
  readonly dek?: string;
  readonly eyebrow?: string;
  readonly heading: string;
  readonly headingId?: string;
  /** Dates, versions, or provenance the reader needs before the body. */
  readonly meta?: ReactNode;
  readonly toc?: readonly SiteDocumentTocItem[];
  readonly tocLabel?: string;
}

/**
 * Reference pages (docs, benchmarks, methodology) use the embedded
 * publication grammar without the essay contract: `MarketingArticle`
 * requires a published date and a provenance record, which reference
 * documentation does not carry. The markup matches the shared grammar so
 * plain-publication.css owns measure, TOC column, tables, and code.
 */
export function SiteDocument({
  children,
  dek,
  eyebrow,
  heading,
  headingId = "document-title",
  meta,
  toc,
  tocLabel = "Contents",
}: Readonly<SiteDocumentProps>) {
  const tocItems = toc ?? [];
  const tocId = `${headingId}-contents`;
  return (
    <article
      aria-labelledby={headingId}
      className="plain-site plain-publication plain-publication--embedded plain-publication__article"
      data-hraness-article=""
      data-toc={tocItems.length > 0 ? "aside" : "none"}
    >
      <header className="plain-publication__article-header">
        {eyebrow === undefined || eyebrow === "" ? null : <p className="plain-publication__eyebrow">{eyebrow}</p>}
        <h1 id={headingId}>{heading}</h1>
        {dek === undefined || dek === "" ? null : <p className="plain-publication__article-dek">{dek}</p>}
        {meta === undefined || meta === null ? null : <p className="plain-publication__article-meta">{meta}</p>}
      </header>
      <div className="plain-publication__article-layout">
        {tocItems.length === 0 ? null : (
          <nav aria-labelledby={tocId} className="plain-publication__toc">
            <p id={tocId}>{tocLabel}</p>
            <ol>
              {tocItems.map((item) => <li key={item.href}><a href={item.href}>{item.label}</a></li>)}
            </ol>
          </nav>
        )}
        <div className="plain-publication__article-body">{children}</div>
      </div>
    </article>
  );
}
