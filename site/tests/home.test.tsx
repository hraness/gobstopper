import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";

import Home from "../app/page";
import Docs from "../app/docs/page";
import BlogIndex from "../app/blog/page";
import { publishedRelease } from "../app/publication";
import RootLayout from "../app/layout";
import Benchmarks from "../app/benchmarks/page";
import { BENCHMARK_STUDIES } from "../app/benchmarks/datasets";
import { plainInline, renderInline } from "../app/_lib/inline";

const SUPPORT_URL = "https://account.hraness.com/support?product=gobstopper&amp;source=web#support";
const SUPPORT_LABEL = "Support ongoing development of Gobstopper, context compaction you can undo.";

function countOccurrences(haystack: string, needle: string): number {
  let count = 0;
  let index = haystack.indexOf(needle);
  while (index !== -1) {
    count += 1;
    index = haystack.indexOf(needle, index + needle.length);
  }
  return count;
}

test("every public route has one optional support footer without product signup", () => {
  for (const Page of [Home, Docs, BlogIndex]) {
    const html = renderToStaticMarkup(<RootLayout><Page /></RootLayout>);
    // The shared support footer appears once; a product marketing footer may also render.
    expect(countOccurrences(html, SUPPORT_URL)).toBe(1);
    expect(html).toContain(SUPPORT_LABEL);
    expect(html).not.toContain('type="email"');
    expect(html).not.toContain('source=web#updates');
  }
});

test("the homepage shares the README identity and installs the guarded source build", () => {
  const html = renderToStaticMarkup(<Home />);
  // Syntax highlighting wraps command tokens in spans; compare visible text.
  const text = html.replace(/<[^>]+>/gu, " ").replace(/\s+/gu, " ");
  expect(html.match(/<h1\b/gu)).toHaveLength(1);
  expect(html).toContain("Before writing a separate Claude Code or Codex copy, Gobstopper archives the original and prepared bytes.");
  expect(text).toContain("cargo install --git https://github.com/hraness/gobstopper gobstopper --locked");
  expect(text).not.toContain("--tag v");
  if (publishedRelease === null) {
    expect(html).toContain("No release yet");
  } else {
    expect(html).toContain(`href="https://github.com/hraness/gobstopper/releases/tag/v${publishedRelease.version}"`);
    expect(html).toContain(publishedRelease.verificationRun);
  }
  expect(html).toMatch(/automatic provider compaction stays disabled/iu);
  expect(html).toContain("Gobstopper does not trigger Claude Code&#x27;s or Codex&#x27;s own compaction and does not edit session files in place.");
  expect(html).not.toContain("hraness.com/gobstopper");
  expect(text).toContain("gobstopper proxy serve");
  expect(html).not.toContain("Source preview");
});

test("the docs page renders the README with its installation anchor", () => {
  const html = renderToStaticMarkup(<Docs />);
  expect(html).toContain('id="install--use"');
  expect(html).toContain('id="integrating-with-a-session-runtime"');
  expect(html.replace(/<[^>]+>/gu, " ").replace(/\s+/gu, " ")).toContain("gobstopper detect");
  expect(html).toContain('data-language="');
  expect(html).not.toContain("data-hraness-marketing-preset");
});

test("the supported-agent strips render shared marks for every routed client", () => {
  const agentNames = ["Claude Code", "Codex", "opencode", "Crush", "Aider", "Goose"];
  const home = renderToStaticMarkup(<Home />);
  const homeChips: string[] = [];
  new HTMLRewriter()
    .on(".gob-agent-marks .hraness-provider-mark__chip", {
      element() { homeChips.push("chip"); },
    })
    .transform(home);
  expect(homeChips).toHaveLength(agentNames.length);
  expect(home).toContain("hraness-provider-mark__art");
  for (const name of agentNames) expect(home).toContain(name);

  const docs = renderToStaticMarkup(<Docs />);
  const docChips: string[] = [];
  new HTMLRewriter()
    .on(".gob-doc-marks .hraness-provider-mark__chip", {
      element() { docChips.push("chip"); },
    })
    .transform(docs);
  expect(docChips).toHaveLength(agentNames.length);
});

test("scopes the editorial preset to the homepage header and real command example", () => {
  const html = renderToStaticMarkup(<Home />);
  const elements: string[] = [];
  new HTMLRewriter()
    .on('[data-hraness-marketing-preset="editorial"] .hraness-marketing-header.hraness-material-chrome', {
      element() { elements.push("header"); },
    })
    .on('[data-hraness-marketing-preset="editorial"] #main .hraness-marketing-data-table', {
      element() { elements.push("proof"); },
    })
    .transform(html);
  expect(elements).toEqual(["header", "proof"]);
  expect(html).toMatch(/autocompact 100<\/th>[\s\S]*?56,300[\s\S]*?data-tone="negative"/u);
  expect(html).not.toContain("--in-place");
  // Retired or nonexistent flags must not appear in homepage examples.
  expect(html).not.toContain("--double-buffer");
  expect(html).not.toMatch(/gobstopper watch[^\n<]*--trigger/u);
  // A preset command is a string and runs only once trusted.
  expect(html).not.toContain("command = [");
  expect(html).toContain("trusted_legacy_command = true");
  expect(html).toContain('href="/docs#install--use"');
  expect(html).toContain("Resume trial on one 333k-token Claude Code session");
});


test("the header keeps a named home link and exact-artwork foil fallback", () => {
  for (const Page of [Home, Docs, BlogIndex]) {
    const html = renderToStaticMarkup(<Page />);
    const homeLinks: string[] = [];
    const marks: string[] = [];
    const fallbackImages: string[] = [];
    const masks: string[] = [];
    new HTMLRewriter()
      .on('header a[aria-label="Gobstopper home"]', {
        element(element) {
          homeLinks.push(element.getAttribute("href") ?? "");
          expect(element.hasAttribute("data-foil")).toBe(true);
        },
      })
      .on('header a[aria-label="Gobstopper home"] .hraness-foil-mark', {
        element(element) { marks.push(element.getAttribute("aria-hidden") ?? ""); },
      })
      .on('header a[aria-label="Gobstopper home"] .hraness-foil-mark img', {
        element(element) {
          fallbackImages.push(element.getAttribute("src") ?? "");
          expect(element.hasAttribute("alt")).toBe(true);
          expect(element.getAttribute("alt") ?? "").toBe("");
        },
      })
      .on('header a[aria-label="Gobstopper home"] .hraness-foil-mark__paint', {
        element(element) { masks.push((element.getAttribute("style") ?? "").replaceAll("&quot;", '"')); },
      })
      .transform(html);
    expect(homeLinks).toEqual(["/"]);
    expect(marks).toEqual(["true"]);
    expect(fallbackImages).toEqual(["/marks/gobstopper.svg"]);
    expect(masks).toEqual(['--hraness-foil-mask:url("/marks/gobstopper.svg")']);
  }
});

test("the homepage names the other ways to shrink context and links each comparison", () => {
  const html = renderToStaticMarkup(<Home />);
  const text = html.replace(/<[^>]+>/gu, " ").replace(/\s+/gu, " ");
  expect(html).toContain('id="alternatives"');
  for (const name of ["Claude Code /compact", "Codex /compact", "CliffCompaction", "RTK"]) {
    expect(text).toContain(name);
  }
  expect(html).toContain('href="/compare/claude-code-compact"');
  expect(html).toContain('href="https://github.com/openai/codex"');
  expect(html).toContain('href="https://github.com/rtk-ai/rtk"');
  expect(text).toMatch(/Checked (January|February|March|April|May|June|July|August|September|October|November|December) \d{1,2}, \d{4}\./u);
  // The CliffCompaction answer links the comparison page instead of naming it in plain text.
  expect(html).toContain('<a href="/compare/cliffcompaction">Gobstopper vs CliffCompaction</a>');
  expect(html).not.toContain("[Gobstopper vs CliffCompaction]");
});

test("the FAQPage structured data carries plain answer text", () => {
  const html = renderToStaticMarkup(<Home />);
  const scripts = [...html.matchAll(/<script type="application\/ld\+json">([\s\S]*?)<\/script>/gu)].map((match) => JSON.parse(match[1] ?? "null"));
  const faq = scripts.find((value) => value?.["@type"] === "FAQPage");
  expect(faq).toBeDefined();
  for (const entry of faq.mainEntity) {
    const answer: string = entry.acceptedAnswer.text;
    expect(answer).not.toContain("`");
    expect(answer).not.toMatch(/\]\(\//u);
  }
  expect(JSON.stringify(faq)).toContain("Gobstopper vs CliffCompaction has the full table");
});

test("inline answer markup renders code and site links, and strips both for structured data", () => {
  const html = renderToStaticMarkup(<p>{renderInline("Run `gobstopper proxy`; see [the table](/compare/cliffcompaction) and [x](https://example.com).")}</p>);
  expect(html).toBe('<p>Run <code>gobstopper proxy</code>; see <a href="/compare/cliffcompaction">the table</a> and [x](https://example.com).</p>');
  expect(plainInline("Run `a`; see [the table](/compare/x).")).toBe("Run a; see the table.");
  expect(plainInline("no markup")).toBe("no markup");
});

test("the related block lists only products with a registered relation", () => {
  const html = renderToStaticMarkup(<Home />);
  expect(html).toContain("Works with Gobstopper.");
  expect(html).toContain('href="https://xcb.sh"');
  expect(html).not.toContain("More from Hraness.");
});

test("the application node names Hraness as publisher and a free offer", () => {
  const html = renderToStaticMarkup(<RootLayout><Home /></RootLayout>);
  const graphs = [...html.matchAll(/<script type="application\/ld\+json">([\s\S]*?)<\/script>/gu)]
    .map((match) => JSON.parse(match[1] ?? "null"))
    .filter((value) => Array.isArray(value?.["@graph"]));
  const app = graphs.flatMap((value) => value["@graph"]).find((node) => node["@type"] === "SoftwareApplication");
  expect(app.publisher).toEqual({
    "@type": "Organization",
    "@id": "https://hraness.com/#organization",
    name: "Hraness",
    url: "https://hraness.com",
  });
  expect(app.offers).toEqual({ "@type": "Offer", price: "0", priceCurrency: "USD" });
});

test("each benchmark Dataset points at a section the benchmarks page renders", () => {
  const html = renderToStaticMarkup(<Benchmarks />);
  for (const study of BENCHMARK_STUDIES) {
    expect(html).toContain(`id="${study.anchor.slice(1)}"`);
  }
  expect(html).toContain('"@type":"Dataset"');
});
