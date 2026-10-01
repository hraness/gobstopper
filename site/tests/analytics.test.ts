import { checkPostHogContract, runPostHogHarness } from "@hraness/posthog/testing";
import { expect, test } from "bun:test";
import { gobstopperPostHogSite } from "../app/_lib/analytics";

test("installed SDK satisfies the observability and private-route contract", () => {
  expect(checkPostHogContract({ site: gobstopperPostHogSite, sensitivePath: "/auth/callback" }).violations).toEqual([]);
});


test("real SDK redacts encoded identifiers before sending paths and exceptions", () => {
  const encodedAddress = "encodedprivacycanary%2540example.com";
  const origin = `https://${gobstopperPostHogSite.canonicalDomain}`;
  const result = runPostHogHarness({
    site: gobstopperPostHogSite,
    scenarios: [
      {
        href: `${origin}/`,
        referrer: "",
        captures: [
          { event: "$pageview" },
          { event: "$exception", error: { name: "TypeError", message: `Failed for ${encodedAddress}, +@a.aa, %2B%40a.aa` } },
        ],
      },
      {
        href: `${origin}/docs/${encodedAddress}`,
        referrer: "",
        captures: [{ event: "$pageview" }],
      },
    ],
  });
  expect(result.sent.some((event) => event.event === "$pageview")).toBe(true);
  expect(result.sent.some((event) => event.event === "$exception")).toBe(true);
  expect(JSON.stringify(result.sent)).not.toContain("encodedprivacycanary");
  expect(JSON.stringify(result.sent)).not.toContain("+@a.aa");
  expect(JSON.stringify(result.sent)).not.toContain("%2B%40a.aa");
});
