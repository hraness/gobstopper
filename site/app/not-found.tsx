import type { Metadata } from "next";

import { SiteHeader, SiteFooter } from "./_components/site-chrome";

export const metadata: Metadata = {
  title: "Not found",
};

export default function NotFound() {
  return (
    <>
      <SiteHeader />
      <main id="main" tabIndex={-1} className="document-page">
        <h1>Page not found</h1>
        <p>
          The page you requested does not exist.{" "}
          <a href="/">Return to Gobstopper home</a> or{" "}
          <a href="/docs">read the docs</a>.
        </p>
      </main>
      <SiteFooter />
    </>
  );
}
