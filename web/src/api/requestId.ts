/**
 * Request identifiers the server put in the `x-request-id` response header.
 *
 * Some services answer errors with a body that carries no RFC 9457
 * `requestId` (for example Access-level diagnostics). The transport records the
 * header against the exact error body or error object it belongs to, so a
 * diagnostic can show the server's identifier. Nothing here ever creates an
 * identifier; when the server sent none, there is none to show.
 */
const headerRequestIds = new WeakMap<object, string>()

export const REQUEST_ID_HEADER = 'x-request-id'

export function rememberHeaderRequestId(target: unknown, requestId: unknown): void {
  if (!target || typeof target !== 'object') return
  if (typeof requestId !== 'string') return
  const value = requestId.trim()
  if (value) headerRequestIds.set(target, value)
}

export function headerRequestId(target: unknown): string | undefined {
  if (!target || typeof target !== 'object') return undefined
  return headerRequestIds.get(target)
}
