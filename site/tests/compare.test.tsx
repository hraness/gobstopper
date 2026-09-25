import { expect, test } from "bun:test";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { renderToStaticMarkup } from "react-dom/server";

import CompareCliffCompaction from "../app/compare/cliffcompaction/page";
import {
  CLIFF_PAPER,
  CLIFF_REPOSITORY,
  comparisonQuestions,
  comparisonRows,
} from "../app/compare/cliffcompaction/comparison";
import RootLayout from "../app/layout";

const SUPPORT_URL = "https://account.hraness.com/support?product=gobstopper&amp;source=web#support";

function cells(line: string): string[] {
  return line.split("|").slice(1, -1).map((cell) => cell.trim());
}

test("the comparison page renders one heading, the shared rows, and primary sources", () => {
  const html = renderToStaticMarkup(<RootLayout><CompareCliffCompaction /></RootLayout>);
  expect(html.match(/<h1\b/gu)).toHaveLength(1);
  expect(html).toContain(`href="${CLIFF_PAPER}"`);
  expect(html).toContain(`href="${CLIFF_REPOSITORY}"`);
  expect(html).toContain(SUPPORT_URL);
  expect(html).toContain("--strategy cliff");
  for (const row of comparisonRows) {
    expect(html).toContain(`<th scope="row">${row.aspect}</th>`);
  }
  for (const { question } of comparisonQuestions) {
    expect(html).toContain(question);
  }
  // Authored copy avoids em dashes; the shared chrome may carry its own text.
  const article = /<article>([\s\S]*)<\/article>/u.exec(html)?.[1] ?? "";
  expect(article).not.toContain("—");
  expect(html).toContain('"@type":"FAQPage"');
});

test("the README comparison table carries the same cells as the page", async () => {
  const readme = await readFile(join(import.meta.dir, "..", "..", "README.md"), "utf8");
  const start = readme.indexOf("## How Gobstopper compares with CliffCompaction");
  expect(start).toBeGreaterThan(-1);
  const section = readme.slice(start, readme.indexOf("\n## ", start + 1));
  const tableLines = section.split("\n").filter((line) => line.startsWith("|"));
  // Header, separator, then one line per row.
  const bodyLines = tableLines.slice(2);
  expect(bodyLines).toHaveLength(comparisonRows.length);
  bodyLines.forEach((line, index) => {
    const row = comparisonRows[index]!;
    expect(cells(line)).toEqual([row.aspect, row.cliff, row.gobstopper]);
  });
  expect(section).toContain("https://gobstopper.sh/compare/cliffcompaction");
});
