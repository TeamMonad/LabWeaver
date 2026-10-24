import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import {
  clearPendingCommand,
  listPendingCommands,
  readPendingCommand,
  syncPendingCommandOwner,
  writePendingCommand,
} from '@/utils/pendingCommands'

const ACTOR = 'actor-1'
const SCOPE = 'manual-work-template:project-1'
const older = { fingerprint: 'draft-a', idempotencyKey: 'K1', data: { draft: 'A' } }
const newer = { fingerprint: 'draft-b', idempotencyKey: 'K2', data: { draft: 'B' } }

describe('pendingCommands memory fallback', () => {
  beforeEach(() => {
    window.sessionStorage.clear()
    syncPendingCommandOwner(null)
  })

  afterEach(() => {
    vi.restoreAllMocks()
    syncPendingCommandOwner(null)
  })

  it('lets a newer memory-only command win over an older stored one even when the stored copy cannot be removed', () => {
    expect(writePendingCommand(ACTOR, SCOPE, older)).toBe(true)
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => { throw new DOMException('full', 'QuotaExceededError') })
    vi.spyOn(Storage.prototype, 'removeItem').mockImplementation(() => { throw new DOMException('denied', 'SecurityError') })

    expect(writePendingCommand(ACTOR, SCOPE, newer)).toBe(false)
    expect(readPendingCommand(ACTOR, SCOPE)?.idempotencyKey).toBe('K2')
    expect(listPendingCommands(ACTOR, 'manual-work-template:')).toEqual([
      { scope: SCOPE, command: newer, persisted: false },
    ])

    // The older request answers late: its key no longer identifies the current command.
    clearPendingCommand(ACTOR, SCOPE, 'K1')
    expect(readPendingCommand(ACTOR, SCOPE)?.idempotencyKey).toBe('K2')

    clearPendingCommand(ACTOR, SCOPE, 'K2')
    expect(listPendingCommands(ACTOR, 'manual-work-template:').map(({ command }) => command.idempotencyKey)).not.toContain('K2')
  })

  it('drops the older stored command when a newer one has to fall back to memory', () => {
    writePendingCommand(ACTOR, SCOPE, older)
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => { throw new DOMException('full', 'QuotaExceededError') })

    expect(writePendingCommand(ACTOR, SCOPE, newer)).toBe(false)
    expect(window.sessionStorage.length).toBe(0)
    clearPendingCommand(ACTOR, SCOPE, 'K1')
    expect(readPendingCommand(ACTOR, SCOPE)).toEqual(newer)
  })

  it('treats a retry of the command already stored under the same key as persisted', () => {
    writePendingCommand(ACTOR, SCOPE, older)
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => { throw new DOMException('full', 'QuotaExceededError') })

    expect(writePendingCommand(ACTOR, SCOPE, older)).toBe(true)
    expect(listPendingCommands(ACTOR, 'manual-work-template:')).toEqual([
      { scope: SCOPE, command: older, persisted: true },
    ])
  })
})
