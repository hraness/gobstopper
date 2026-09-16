/** Bind installation coordinates to the admitted release, preserving all other prose. */
export function publishedReadme(source: string, sourceVersion: string, publishedVersion: string | null): string {
  if (publishedVersion === null) {
    throw new Error("Cannot publish README installation commands without an admitted release.");
  }
  for (const version of [sourceVersion, publishedVersion]) {
    if (!/^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/u.test(version)
      || version.split(".").some((part) => BigInt(part) > BigInt(Number.MAX_SAFE_INTEGER))) {
      throw new TypeError("README installation version must be a canonical stable version.");
    }
  }
  const escaped = sourceVersion.replaceAll(".", "\\.");
  return source
    .replace(new RegExp(`--tag v${escaped}(?![\\w.-])`, "gu"), `--tag v${publishedVersion}`)
    .replace(new RegExp(`hraness/gobstopper#v${escaped}(?![\\w.-])`, "gu"), `hraness/gobstopper#v${publishedVersion}`);
}
