import { describe, expect, test } from "bun:test";
import { join } from "node:path";

import { publishedRelease } from "../app/publication";

const site = join(import.meta.dir, "..");

async function startBuiltSite() {
  const process_ = Bun.spawn([
    join(site, "node_modules/.bin/next"),
    "start",
    "--hostname",
    "127.0.0.1",
    "--port",
    "0",
  ], {
    cwd: site,
    env: { ...process.env, NODE_ENV: "production" },
    stderr: "pipe",
    stdout: "pipe",
  });
  let output = "";
  let startupSettled = false;
  let rejectStartup: (error: Error) => void = () => {};
  let resolveStartup: (origin: string) => void = () => {};
  const startup = new Promise<string>((resolve, reject) => {
    rejectStartup = reject;
    resolveStartup = resolve;
  });
  const settleFromOutput = (): void => {
    const match = output.match(/http:\/\/127\.0\.0\.1:(\d+)/u);
    if (match === null || !output.includes("Ready in") || startupSettled) return;
    startupSettled = true;
    resolveStartup(`http://127.0.0.1:${match[1]}`);
  };
  const capture = async (stream: ReadableStream<Uint8Array>): Promise<void> => {
    const decoder = new TextDecoder();
    const reader = stream.getReader();
    try {
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        output += decoder.decode(value, { stream: true });
        settleFromOutput();
      }
      output += decoder.decode();
      settleFromOutput();
    } catch (error) {
      if (!startupSettled) {
        startupSettled = true;
        rejectStartup(error instanceof Error ? error : new Error(String(error)));
      }
    } finally {
      reader.releaseLock();
    }
  };
  const captureTasks = [capture(process_.stdout), capture(process_.stderr)];
  const exitTask = process_.exited.then((exitCode) => {
    if (startupSettled) return;
    startupSettled = true;
    rejectStartup(new Error(`Next exited with code ${exitCode} before startup.\n${output}`));
  });
  const timeout = setTimeout(() => {
    if (startupSettled) return;
    startupSettled = true;
    rejectStartup(new Error(`Next did not start within 10 seconds.\n${output}`));
  }, 10_000);
  try {
    const origin = await startup;
    clearTimeout(timeout);
    return { captureTasks, exitTask, origin, process_ };
  } catch (error) {
    clearTimeout(timeout);
    if (process_.exitCode === null) process_.kill("SIGTERM");
    await process_.exited;
    await Promise.allSettled(captureTasks);
    throw error;
  }
}

async function stopBuiltSite(server: Awaited<ReturnType<typeof startBuiltSite>>): Promise<void> {
  if (server.process_.exitCode === null) server.process_.kill("SIGTERM");
  const stoppedGracefully = await Promise.race([
    server.process_.exited.then(() => true),
    Bun.sleep(2_000).then(() => false),
  ]);
  if (!stoppedGracefully && server.process_.exitCode === null) {
    server.process_.kill("SIGKILL");
    await server.process_.exited;
  }
  await server.exitTask;
  await Promise.allSettled(server.captureTasks);
}

describe("built Gobstopper site", () => {
  test("serves public pages, recovery evidence, and discovery files through Next", async () => {
    const server = await startBuiltSite();
    try {
      const [homeResponse, docsResponse, robotsResponse, llmsResponse, cardResponse, docsCardResponse, missingResponse, benchmarksResponse, recoveryResultsResponse, recoveryProtocolResponse] = await Promise.all([
        fetch(`${server.origin}/`, { redirect: "manual" }),
        fetch(`${server.origin}/docs`, { redirect: "manual" }),
        fetch(`${server.origin}/robots.txt`, { redirect: "manual" }),
        fetch(`${server.origin}/llms.txt`, { redirect: "manual" }),
        fetch(`${server.origin}/opengraph-image`, { redirect: "manual" }),
        fetch(`${server.origin}/docs/opengraph-image`, { redirect: "manual" }),
        fetch(`${server.origin}/missing`, { redirect: "manual" }),
        fetch(`${server.origin}/benchmarks`, { redirect: "manual" }),
        fetch(`${server.origin}/benchmarks/2026-09-20/recovery-study-results.json`, { redirect: "manual" }),
        fetch(`${server.origin}/benchmarks/2026-09-20/recovery-study-protocol.json`, { redirect: "manual" }),
      ]);
      const [rawHome, rawDocs, robots, llms] = await Promise.all([homeResponse.text(), docsResponse.text(), robotsResponse.text(), llmsResponse.text()]);
      const home = rawHome.replaceAll(/https:\/\/[a-z0-9-]+\.vercel\.app/gu, "https://gobstopper.sh");
      const docs = rawDocs.replaceAll(/https:\/\/[a-z0-9-]+\.vercel\.app/gu, "https://gobstopper.sh");
      expect(homeResponse.status).toBe(200);
      expect(home).toContain("cargo install --git https://github.com/hraness/gobstopper gobstopper --locked");
      if (publishedRelease === null) {
        expect(home).toContain("No release yet");
      } else {
        expect(home).toContain(`href="https://github.com/hraness/gobstopper/releases/tag/v${publishedRelease.version}"`);
        expect(home).toContain(publishedRelease.verificationRun);
      }
      expect(home).toContain('<link rel="canonical" href="https://gobstopper.sh"');
      expect(home).toContain('aria-label="Ask AI about this"');
      expect(home).toMatch(/<meta\s+property="og:image"\s+content="https:\/\/gobstopper\.sh\/opengraph-image(?:\?[^"]+)?"/u);
      expect(home).toContain('<meta name="twitter:card" content="summary_large_image"');
      expect(home).toMatch(/<meta\s+name="twitter:image"\s+content="https:\/\/gobstopper\.sh\/opengraph-image(?:\?[^"]+)?"/u);
      expect(docsResponse.status).toBe(200);
      expect(docs).toContain('<link rel="canonical" href="https://gobstopper.sh/docs"');
      expect(docs).toContain('id="install--use"');
      expect(docs).toMatch(/<meta\s+property="og:image"\s+content="https:\/\/gobstopper\.sh\/docs\/opengraph-image(?:\?[^"]+)?"/u);
      expect(robotsResponse.status).toBe(200);
      expect(robots).toContain("Sitemap: https://gobstopper.sh/sitemap.xml");
      expect(llmsResponse.status).toBe(200);
      expect(llms).toContain("https://gobstopper.sh/docs");
      expect(cardResponse.status).toBe(200);
      expect(cardResponse.headers.get("content-type")).toContain("image/png");
      expect(docsCardResponse.status).toBe(200);
      expect(docsCardResponse.headers.get("content-type")).toContain("image/png");
      expect(missingResponse.status).toBe(404);
      expect(benchmarksResponse.status).toBe(200);
      expect(await benchmarksResponse.text()).toContain('id="archived-recovery-2026-09-20"');
      for (const [response, file] of [
        [recoveryResultsResponse, "recovery-study-results.json"],
        [recoveryProtocolResponse, "recovery-study-protocol.json"],
      ] as const) {
        expect(response.status).toBe(200);
        expect(response.headers.get("content-type")).toContain("application/json");
        expect(await response.text()).toBe(await Bun.file(join(site, "public/benchmarks/2026-09-20", file)).text());
      }
    } finally {
      await stopBuiltSite(server);
    }
  }, 20_000);
  test("serves the blog, its posts, the Atom feed, and indexable sitemap entries", async () => {
    const server = await startBuiltSite();
    try {
      const canonical = (html: string): string => html.replaceAll(/https:\/\/[a-z0-9-]+\.vercel\.app/gu, "https://gobstopper.sh");
      const index = await fetch(`${server.origin}/blog`, { redirect: "manual" });
      expect(index.status).toBe(200);
      const indexHtml = canonical(await index.text());
      expect(indexHtml).toContain('<link rel="canonical" href="https://gobstopper.sh/blog"');
      expect(indexHtml).toContain('"@type":"Blog"');
      for (const slug of ["introducing-gobstopper", "proofs-for-the-admission-math", "vault-models-that-fail-on-purpose"]) {
        const response = await fetch(`${server.origin}/blog/${slug}`, { redirect: "manual" });
        expect(response.status).toBe(200);
        const html = canonical(await response.text());
        expect(html).toContain(`<link rel="canonical" href="https://gobstopper.sh/blog/${slug}"`);
        expect(html).toContain('"@type":"BlogPosting"');
        expect(html).toContain("Drafted with AI from the source code and reviewed by Claude Opus 5.5 (claude-opus-5-5) editorial review.");
        expect(html).toMatch(/<meta\s+property="og:type"\s+content="article"/u);
        expect(html).not.toMatch(/<meta\s+name="robots"\s+content="noindex/u);
        const card = await fetch(`${server.origin}/blog/${slug}/opengraph-image`, { redirect: "manual" });
        expect(card.status).toBe(200);
        expect(card.headers.get("content-type")).toContain("image/png");
      }
      const missing = await fetch(`${server.origin}/blog/not-a-post`, { redirect: "manual" });
      expect(missing.status).toBe(404);
      const feed = await fetch(`${server.origin}/blog/feed.xml`, { redirect: "manual" });
      expect(feed.status).toBe(200);
      expect(feed.headers.get("content-type")).toContain("application/atom+xml");
      expect(await feed.text()).toContain("<id>https://gobstopper.sh/blog/introducing-gobstopper</id>");
      const sitemap = await (await fetch(`${server.origin}/sitemap.xml`, { redirect: "manual" })).text();
      expect(sitemap).toContain("<loc>https://gobstopper.sh/blog/introducing-gobstopper</loc>");
      expect(sitemap).toContain("<lastmod>2026-09-24T00:00:00.000Z</lastmod>");
    } finally {
      await stopBuiltSite(server);
    }
  }, 20_000);
});
