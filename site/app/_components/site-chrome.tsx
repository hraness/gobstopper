import { HranessSiteFooter } from "@hraness/site-footer/react";
import {
  MarketingSiteFooter,
  MarketingSiteHeader,
} from "@hraness/design-kit/react/server";
import { ThemeMenuButton } from "@hraness/design-kit/react";
import { AskAiAboutThis } from "@hraness/ui";

import {
  absoluteUrl,
  type CanonicalPagePath,
  GITHUB_URL,
  ARCHITECTURE_URL,
} from "../_lib/site";
import { supportProfile } from "../support-profile";

// eslint-disable-next-line @next/next/no-img-element
const productMark = <img alt="" height={20} src="/icon.png" width={20} />;

export function SiteHeader({ path }: Readonly<{ path?: CanonicalPagePath }>) {
  return (
    <>
      <a className="skip-link" href="#main">
        Skip to content
      </a>
      <MarketingSiteHeader
        action={{ href: "/#install", label: "Install Gobstopper" }}
        ariaLabel="Primary navigation"
        trailing={<ThemeMenuButton aria-label="Appearance" />}
        brand="Gobstopper"
        brandMark="/marks/gobstopper.svg"
        brandLabel="Gobstopper home"
        className={
          path === "/"
            ? "site-header hraness-material-chrome"
            : "site-header"
        }
        links={[
          { href: "/#model", label: "Model" },
          { href: "/#interfaces", label: "Interfaces" },
          { href: "/docs", label: "Docs" },
          { href: "/methodology", label: "Methodology" },
          { href: "/benchmarks", label: "Benchmarks" },
          { href: "/compare/cliffcompaction", label: "Compare" },
          { href: ARCHITECTURE_URL, label: "Architecture" },
          { href: GITHUB_URL, label: "GitHub" },
        ]}
      />
    </>
  );
}

export function SiteFooter({ path }: Readonly<{ path?: CanonicalPagePath }>) {
  return (
    <>
      {path === undefined ? null : (
        <AskAiAboutThis className="ask-ai" url={absoluteUrl(path)} />
      )}
      <MarketingSiteFooter
        ariaLabel="Gobstopper"
        brand={productMark}
        brandHref="/"
        brandLabel="Gobstopper home"
        links={[
          { href: "/docs", label: "Docs" },
          { href: "/methodology", label: "Methodology" },
          { href: "/benchmarks", label: "Benchmarks" },
          { href: "/compare/cliffcompaction", label: "Compare" },
          { href: ARCHITECTURE_URL, label: "Architecture" },
          { href: GITHUB_URL, label: "GitHub" },
        ]}
        name="Gobstopper"
      >
        <p>Built for coding agents · MIT source · in development</p>
      </MarketingSiteFooter>
      <div className="network-footer">
        <HranessSiteFooter
          placement="flow"
          mailingList={{ kind: "none" }}
          support={supportProfile}
        />
      </div>
    </>
  );
}
