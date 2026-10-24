/**
 * Explicit model-input file selection (#216).
 *
 * `selectedPaths` carries the exact `PackageFile.path` strings the user ticked.
 * Paths are never trimmed or normalized: the contract accepts leading and
 * trailing spaces and Control matches the exact identity. The result keeps the
 * package's own file order so the same choice always produces the same request
 * body, and it never falls back to the whole package.
 */
export function buildSelectedPaths(selected: readonly string[], packagePaths: readonly string[]): string[] {
  const chosen = new Set(selected)
  const seen = new Set<string>()
  const paths: string[] = []
  for (const path of packagePaths) {
    if (!chosen.has(path) || seen.has(path)) continue
    seen.add(path)
    paths.push(path)
  }
  return paths
}
