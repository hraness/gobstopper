"use client";

import { attachFoil } from "@hraness/design-kit/browser";
import { usePathname } from "next/navigation";
import { useEffect } from "react";

/** The network footer owns its own foil enhancement. */
export function FoilController() {
  const pathname = usePathname();
  useEffect(() => {
    const cleanups = Array.from(
      document.querySelectorAll<HTMLElement>("header.hraness-marketing-header, footer.hraness-marketing-footer"),
      (element) => attachFoil(element),
    );
    return () => cleanups.forEach((cleanup) => cleanup());
  }, [pathname]);
  return null;
}
