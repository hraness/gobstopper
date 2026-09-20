"use client";

import { attachFoil } from "@hraness/design-kit/browser";
import { useEffect } from "react";

export function FoilController() {
  useEffect(() => attachFoil(document.documentElement), []);
  return null;
}
