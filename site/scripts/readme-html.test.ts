import { describe, expect, test } from "bun:test";
import { readFile } from "node:fs/promises";
import { join } from "node:path";

import { documentSections, headingIds, LANDING_END, LANDING_START, readmeLanding, renderReadmeHtml } from "./readme-html.ts";
import { publishedReadme } from "./published-readme.ts";
import {
  createSiteLinkResolver,
  markdownSection,
  PROXY_GUIDE_SOURCE,
  renderProxyGuideHtml,
  SERVICE_SOURCE,
} from "./site-documents.ts";

const repository = join(import.meta.dir, "..", "..");

test("versioned installation references use the admitted release without changing source installs", () => {
  const source = [
    "cargo install --git https://github.com/hraness/gobstopper --tag v0.21.1 gobstopper",
    "cargo install --git https://github.com/hraness/gobstopper gobstopper --locked",
    "bunx skills add hraness/gobstopper#v0.21.1",
    "Version 0.21.1 and historical v0.20.0 remain prose.",
    "Unrelated hraness/gobstopper#v0.21.10 and hraness/gobstopper#v0.21.1-beta.1 stay literal.",
  ].join("\n");
  const projected = publishedReadme(source, "0.21.1", "0.21.0");
  expect(projected).toContain("--tag v0.21.0 gobstopper");
  expect(projected).toContain("cargo install --git https://github.com/hraness/gobstopper gobstopper --locked");
  expect(projected).toContain("hraness/gobstopper#v0.21.0");
  expect(projected).toContain("Version 0.21.1 and historical v0.20.0 remain prose.");
  expect(projected).toContain("Unrelated hraness/gobstopper#v0.21.10 and hraness/gobstopper#v0.21.1-beta.1 stay literal.");
  expect(publishedReadme(source, "0.21.1", "0.21.1")).toBe(source);
  expect(() => publishedReadme(source, "0.21.1", "latest")).toThrow();
  expect(() => publishedReadme(source, "0.21.1", null)).toThrow("without an admitted release");
});

test("renders the repository README with stable heading fragments and repository-rooted relative links", async () => {
  const source = await readFile(join(repository, "README.md"), "utf8");
  const html = renderReadmeHtml(source);
  expect(html).toContain('<h2 id="install--use">Install &amp; use</h2>');
  expect(html).toContain('<h2 id="integrating-with-a-session-runtime">Integrating with a session runtime</h2>');
  expect(html).toContain('href="https://github.com/hraness/gobstopper/blob/main/docs/design.md"');
  expect(html).not.toContain("<script");
});

test("extracts the landing block between the shared Hraness markers", async () => {
  const source = await readFile(join(repository, "README.md"), "utf8");
  expect(source.indexOf(LANDING_START)).toBeGreaterThanOrEqual(0);
  expect(source.indexOf(LANDING_END)).toBeGreaterThan(source.indexOf(LANDING_START));
  const landing = readmeLanding(source);
  expect(landing.title).toBe("Gobstopper");
  expect(landing.lead).toStartWith("🍬 Gobstopper saves tokens while preserving context.");
  expect(landing.lead).not.toContain(">");
  expect(landing.markdown).toMatch(/cannot ask providers to compact,\s+even when `auto_compact_closed` is enabled/u);
});

test("rejects unsafe README link targets", () => {
  expect(() => renderReadmeHtml("[x](javascript:alert(1))")).toThrow("disallowed URL scheme");
  expect(() => renderReadmeHtml("[x](//evil.example)")).toThrow("protocol-relative");
  expect(() => renderReadmeHtml("[x](#missing)")).toThrow("no rendered heading");
});


test("omits repository landing markers and keeps real links durable", async () => {
  const source = await Bun.file(new URL("../../README.md", import.meta.url)).text();
  const html = renderReadmeHtml(source);
  expect(html).not.toContain("hraness:gobstopper-landing");
  expect(html).toContain('href="https://github.com/hraness/gobstopper/blob/main/docs/roadmap.md"');
});


describe("README HTML boundary", () => {
  test("derives stable fragments from parsed heading text", () => {
    const html = renderReadmeHtml([
      "## **Hello** &amp; `world`",
      "## **Hello** &amp; `world`",
      "[First](#hello--world) [Again](#hello--world-1)",
    ].join("\n\n"));
    expect(html).toContain('<h2 id="hello--world"><strong>Hello</strong> &amp; <code>world</code></h2>');
    expect(html).toContain('<h2 id="hello--world-1">');
  });

  test("keeps raw and nested malformed HTML inert, including inside headings", () => {
    const payloads = [
      '<script>alert(1)</script>',
      '<sc<script>ript>alert(1)</sc</script>ript>',
      '<img src="x" onerror="alert(1)">',
      '<svg onload="alert(1)"><a href="javascript:alert(1)">x</a></svg>',
      '<textarea><img src=x onerror=alert(1)></textarea>',
    ];
    for (const payload of payloads) {
      const html = renderReadmeHtml(`## Literal ${payload}\n\n${payload}`);
      const elements: string[] = [];
      const ids: string[] = [];
      new HTMLRewriter().on("*", {
        element(element) {
          elements.push(element.tagName);
          for (const [name] of element.attributes) expect(name).not.toMatch(/^on/iu);
          const id = element.getAttribute("id");
          if (id !== null) ids.push(id);
        },
      }).transform(html);
      expect(elements).toEqual(["h2", "p"]);
      expect(ids).toHaveLength(1);
      expect(ids[0]).toMatch(/^[\p{Letter}\p{Mark}\p{Number}_-]+$/u);
      expect(html).toContain("&lt;");
    }
  });

  test("rejects executable and protocol-relative Markdown URLs", () => {
    for (const target of ["javascript:alert", "java&#x73;cript:alert", "data:text/html,bad", "//example.com"]) {
      expect(() => renderReadmeHtml(`[link](${target})`)).toThrow();
      expect(() => renderReadmeHtml(`![image](${target})`)).toThrow();
    }
    expect(renderReadmeHtml("[Reference](docs/example.md)")).toContain(
      'href="https://github.com/hraness/gobstopper/blob/main/docs/example.md"',
    );
  });
});


describe("proxy guide at /docs/proxy", () => {
  const read = (path: string) => readFile(join(repository, path), "utf8");

  test("composes docs/proxy.md with the diagnosis and direct-fallback sections of docs/service.md", async () => {
    const [proxy, service] = await Promise.all([read(PROXY_GUIDE_SOURCE), read(SERVICE_SOURCE)]);
    const html = renderProxyGuideHtml(proxy, service);
    const sections = documentSections(html).map((section) => section.href);
    expect(sections).toContain("#troubleshooting");
    expect(sections.slice(-2)).toEqual(["#diagnose-repair-and-remove", "#launch-with-a-direct-fallback"]);
    // Other docs/service.md sections stay on GitHub.
    expect(html).not.toContain('id="install-and-inspect"');
    expect(html).toContain('href="https://github.com/hraness/gobstopper/blob/main/docs/service.md#install-and-inspect"');
    expect(html).toContain('href="#launch-with-a-direct-fallback"');
    expect(html).not.toContain("blob/main/docs/proxy.md");
    // Images and links resolve from docs/, not the repository root.
    expect(html).toContain('src="https://raw.githubusercontent.com/hraness/gobstopper/main/docs/assets/gob-anatomy.png"');
    expect(html).toContain('href="https://github.com/hraness/gobstopper/blob/main/docs/context-retention.md"');
    expect(html).toContain('href="/docs#what-gobstopper-has-measured"');
  });

  test("keeps the Chat Completions limit for opencode, Crush, Aider, and Goose verbatim", async () => {
    const proxy = await read(PROXY_GUIDE_SOURCE);
    const limit = "Chat Completions coverage is tested against synthetic histories and recorded contracts, not a live opencode, Crush, Aider, or Goose session; provider acceptance of that dialect is unqualified.";
    expect(proxy.replace(/\s+/gu, " ")).toContain(limit);
    const html = renderProxyGuideHtml(proxy, await read(SERVICE_SOURCE));
    expect(html.replace(/\s+/gu, " ")).toContain(limit);
  });

  test("links between the README and the guide land on headings that exist", async () => {
    const [readme, proxy, service] = await Promise.all([read("README.md"), read(PROXY_GUIDE_SOURCE), read(SERVICE_SOURCE)]);
    const readmeHtml = renderReadmeHtml(readme, { resolveSiteLink: createSiteLinkResolver(service) });
    const guideHtml = renderProxyGuideHtml(proxy, service);
    expect(readmeHtml).not.toContain("blob/main/docs/proxy.md");
    expect(readmeHtml).toContain('href="/docs/proxy"');
    expect(readmeHtml).toContain('href="/docs/proxy#diagnose-repair-and-remove"');
    const guideIds = headingIds(guideHtml);
    for (const [, fragment] of readmeHtml.matchAll(/href="\/docs\/proxy#([^"]+)"/gu)) {
      expect(guideIds.has(fragment ?? "")).toBe(true);
    }
    const readmeIds = headingIds(readmeHtml);
    for (const [, fragment] of guideHtml.matchAll(/href="\/docs#([^"]+)"/gu)) {
      expect(readmeIds.has(fragment ?? "")).toBe(true);
    }
  });

  test("reads level-2 sections through their subsections and skips headings inside code", () => {
    const source = ["# Title", "## One", "text", "```sh", "## not a heading", "```", "### Sub", "more", "## Two", "end"].join("\n");
    expect(markdownSection(source, "One")).toBe(["## One", "text", "```sh", "## not a heading", "```", "### Sub", "more"].join("\n"));
    expect(markdownSection(source, "Two")).toBe("## Two\nend");
    expect(() => markdownSection(source, "Three")).toThrow("has no section");
  });

  test("resolves relative links from the document's directory and refuses links outside the repository", () => {
    const html = renderReadmeHtml("[Notices](../THIRD_PARTY_NOTICES.md) [Budgets](context-budgets.md#scopes)", { baseDirectory: "docs" });
    expect(html).toContain('href="https://github.com/hraness/gobstopper/blob/main/THIRD_PARTY_NOTICES.md"');
    expect(html).toContain('href="https://github.com/hraness/gobstopper/blob/main/docs/context-budgets.md#scopes"');
    expect(() => renderReadmeHtml("[Outside](../../secret.md)", { baseDirectory: "docs" })).toThrow("leaves the repository");
  });
});
