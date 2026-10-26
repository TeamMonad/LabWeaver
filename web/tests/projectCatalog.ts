import { useProjects } from '@/composables/useProjects'

/**
 * Start a test from an unread shared project catalog, as a freshly opened page
 * does. The catalog is module state and a mounting consumer reads it only while
 * it is unread or failed, so a test that answers `listProjects` itself resets
 * the catalog before mounting instead of inheriting an earlier test's list.
 */
export function resetProjectCatalog(): void {
  const projects = useProjects()
  projects.projects = { kind: 'idle' }
  projects.settledProjects = { kind: 'idle' }
}

/**
 * Read the catalog from the mocked `listProjects` before a view mounts, the way
 * the AppShell has read it before it renders a project-scoped page.
 */
export async function preloadProjectCatalog(): Promise<void> {
  resetProjectCatalog()
  await useProjects().load()
}
