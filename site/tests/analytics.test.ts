import { checkPostHogContract } from "@hraness/posthog/testing";
import { expect, test } from "bun:test";
import { gobstopperPostHogSite } from "../app/_lib/analytics";

test("installed SDK satisfies the observability and private-route contract", () => {
  expect(checkPostHogContract({ site: gobstopperPostHogSite, sensitivePath: "/auth/callback" }).violations).toEqual([]);
});
