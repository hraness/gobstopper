<!-- BEGIN:nextjs-agent-rules -->

# This is NOT the Next.js you know

This version has breaking changes — APIs, conventions, and file structure may all differ from your training data. Read the relevant guide in `node_modules/next/dist/docs/` (resolved from this file's directory; in monorepos the `next` package may not be visible from the repo root) before writing any code. Heed deprecation notices.

This block is written and re-added by `next dev` — verify at `node_modules/next/dist/server/lib/generate-agent-files.js`. Removing it from a diff only re-creates the uncommitted change; committing it with your work keeps the tree clean.

<!-- END:nextjs-agent-rules -->

# Share images

- Share images come only from the shared `@hraness/web-discovery` social-image template, through the single `defineSocialImageSite` declaration in `app/social.ts`. Pages pass copy only (`headline`, `description`, `eyebrow`); no per-site drawing code.

# Browser check before layout pushes

- Most site CI failures are layout asserts in `Verify public routes and appearance` (tap-target size, full-page width at 360px, browser runtime errors). Before pushing a change to layout, CSS, or routes, run `bun run build && bun run check:public-site:browser` from `site/`. It starts its own Next server on a random port and uses the system Chrome. On a shared Hraness Mac, run it in the host scheduler's `browser-auth` lane.
- Copy-only changes can rely on CI. Pull requests that touch only the site skip the Rust gates, so the site result arrives in about two minutes.
- `GOBSTOPPER_BROWSER_CONTEXTS` sets how many (width, theme) contexts run at once (default 3). Set it to 1 to reproduce a failure serially; failure screenshots are saved per context as `failure-<width>-<theme>.png`.

- Website names, product descriptions, hero copy, and named headings read the repository-root `portfolio-messaging.generated.json` projection of `https://hraness.com/portfolio.json`. Edit the canonical Jungle portfolio registry and refresh that snapshot; ordinary builds never fetch or rewrite it.
