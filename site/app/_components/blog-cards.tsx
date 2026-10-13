import type { BlogPost } from "../blog/articles";
import { postPath } from "../blog/articles";

const DATE_FORMAT = new Intl.DateTimeFormat("en-US", { day: "numeric", month: "long", year: "numeric", timeZone: "UTC" });

function PostDate({ label, value }: Readonly<{ label: string; value: string }>) {
  return (
    <>
      {label} <time dateTime={value}>{DATE_FORMAT.format(new Date(`${value}T00:00:00Z`))}</time>
    </>
  );
}

/**
 * The blog index as cards: each post's share image beside its eyebrow, title, summary, and dates.
 * The markup keeps the publication list grammar (`plain-publication__entry`) so the shared
 * stylesheet owns measure and type; the card image and grid are site-owned.
 */
export function BlogCards({
  heading,
  headingId,
  posts,
  summary,
}: Readonly<{ heading: string; headingId: string; posts: readonly BlogPost[]; summary: string }>) {
  return (
    <section aria-labelledby={headingId} className="plain-site plain-publication plain-publication--embedded plain-publication__list gob-blog" data-hraness-article-index="">
      <div className="plain-publication__section-heading">
        <h1 id={headingId}>{heading}</h1>
        <p>{summary}</p>
      </div>
      <div className="plain-publication__article-list gob-blog__list">
        {posts.map((post) => {
          const href = postPath(post);
          return (
            <article className="plain-publication__entry gob-blog__card" key={post.slug}>
              <a aria-hidden="true" className="gob-blog__art" href={href} tabIndex={-1}>
                {/* The share image is a static PNG route; next/image adds nothing for a fixed 1200x630 card. */}
                {/* eslint-disable-next-line @next/next/no-img-element */}
                <img alt="" decoding="async" height={630} loading="lazy" src={`${href}/opengraph-image`} width={1200} />
              </a>
              <div className="gob-blog__copy">
                <p className="plain-publication__entry-label">{post.eyebrow}</p>
                <h2 className="plain-publication__entry-title">
                  <a href={href}>{post.title}</a>
                </h2>
                <p className="plain-publication__entry-dek">{post.dek}</p>
                <p className="plain-publication__entry-meta">
                  <PostDate label="Published" value={post.published} />
                  {post.updated === undefined ? null : (
                    <>
                      {" · "}
                      <PostDate label="Updated" value={post.updated} />
                    </>
                  )}
                </p>
              </div>
            </article>
          );
        })}
      </div>
    </section>
  );
}
