import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";

import Home from "../app/page";
import Docs from "../app/docs/page";
import { publishedRelease } from "../app/publication";
import RootLayout from "../app/layout";

const SUPPORT_URL = "https://account.hraness.com/support?product=gobstopper&amp;source=web#support";
const SUPPORT_LABEL = "Support ongoing development of earlier, smarter context compaction for coding agents.";

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
  for (const Page of [Home, Docs]) {
    const html = renderToStaticMarkup(<RootLayout><Page /></RootLayout>);
    // The shared support footer appears once; a product marketing footer may also render.
    expect(countOccurrences(html, SUPPORT_URL)).toBe(1);
    expect(html).toContain(SUPPORT_LABEL);
    expect(html).not.toContain('type="email"');
    expect(html).not.toContain('source=web#updates');
  }
});

test("the homepage leads with the README identity and the verified install command", () => {
  const html = renderToStaticMarkup(<Home />);
  expect(html.match(/<h1\b/gu)).toHaveLength(1);
  expect(html).toContain("Compact, resume, and audit every agent session.");
  if (publishedRelease === null) {
    expect(html).toContain("First Gobstopper release in preparation");
    expect(html).not.toContain("--tag v");
  } else {
    expect(html).toContain(`--tag v${publishedRelease.version}`);
    expect(html).toContain("cargo install --git");
    expect(html).toContain(publishedRelease.verificationRun);
  }
  expect(html).not.toContain("hraness.com/gobstopper");
});

test("the docs page renders the README with its installation anchor", () => {
  const html = renderToStaticMarkup(<Docs />);
  expect(html).toContain('id="install--use"');
  expect(html).toContain('id="the-oompa-seam"');
  expect(html).toContain("gobstopper detect");
  expect(html).not.toContain("data-hraness-marketing-preset");
});

test("scopes the editorial preset to the homepage header and real command example", () => {
  const html = renderToStaticMarkup(<Home />);
  const elements: string[] = [];
  new HTMLRewriter()
    .on('[data-hraness-marketing-preset="editorial"] .hraness-marketing-header.hraness-material-chrome', {
      element() { elements.push("header"); },
    })
    .on('[data-hraness-marketing-preset="editorial"] #main .hraness-material-wall .hraness-marketing-proof-frame.hraness-material-pane', {
      element() { elements.push("proof"); },
    })
    .transform(html);
  expect(elements).toEqual(["header", "proof"]);
  expect(html).toContain("gobstopper apply 034... --in-place --strategy compacted");
  expect(html).toContain("On a 333k-token Claude session");
});


test("the header keeps a named home link and exact-artwork foil fallback", () => {
  for (const Page of [Home, Docs]) {
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
