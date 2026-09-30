"use client";

import { PostHogPageNotFound } from "@hraness/posthog/react";
import { gobstopperPostHogSite } from "./_lib/analytics";

export function NotFoundAnalytics() {
  return <PostHogPageNotFound
    apiKey={process.env.NEXT_PUBLIC_POSTHOG_KEY}
    apiHost={process.env.NEXT_PUBLIC_POSTHOG_HOST}
    site={gobstopperPostHogSite}
  />;
}
