import {
  listProjectResourceLeases,
  listProjectResourceRequests,
  listResourceGpuCatalog,
  listResourceRates,
} from '@/generated/contracts'
import type {
  GpuCatalogEntrySchema,
  ResourceLeaseSchema,
  ResourceRateSchema,
  ResourceRequestSchema,
} from '@/generated/contracts'
import { collectCursorPages } from '@/utils/cursorPagination'

/** Largest page Resource accepts (`ResourceListQuery.limit` is 1..100). */
export const RESOURCE_PAGE_LIMIT = 100

export function resourcePageQuery(cursor: string | undefined, limit = RESOURCE_PAGE_LIMIT) {
  return { limit, ...(cursor ? { cursor } : {}) }
}

/** Every request of one project, following the cursor to the end. */
export async function fetchAllProjectResourceRequests(projectId: string): Promise<ResourceRequestSchema[]> {
  const page = await collectCursorPages<ResourceRequestSchema>((cursor) => listProjectResourceRequests({
    path: { projectId },
    query: resourcePageQuery(cursor),
  }))
  return page.items
}

/** Every lease of one project, following the cursor to the end. */
export async function fetchAllProjectResourceLeases(projectId: string): Promise<ResourceLeaseSchema[]> {
  const page = await collectCursorPages<ResourceLeaseSchema>((cursor) => listProjectResourceLeases({
    path: { projectId },
    query: resourcePageQuery(cursor),
  }))
  return page.items
}

/** Every GPU catalog revision, active or not, following the cursor to the end. */
export async function fetchAllGpuCatalogEntries(): Promise<GpuCatalogEntrySchema[]> {
  const page = await collectCursorPages<GpuCatalogEntrySchema>((cursor) => listResourceGpuCatalog({ query: resourcePageQuery(cursor) }))
  return page.items
}

/** Every Resource rate version, following the cursor to the end. */
export async function fetchAllResourceRates(): Promise<ResourceRateSchema[]> {
  const page = await collectCursorPages<ResourceRateSchema>((cursor) => listResourceRates({ query: resourcePageQuery(cursor) }))
  return page.items
}
