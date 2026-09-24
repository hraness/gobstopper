import { initDesignPalette } from "@hraness/design-kit/browser";
import { siteDefaultPalette } from "../palette";

// Bundled as a same-origin classic script and executed before the page paints.
// The site's default stays its Tokyo Night identity following the operating system.
initDesignPalette({
  defaultPreference: siteDefaultPalette,
});
