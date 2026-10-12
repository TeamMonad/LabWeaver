import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { effectScope, ref } from 'vue'
import { flushPromises } from '@vue/test-utils'
import { useProjectProblemPackageUpload } from '@/composables/useProjectProblemPackageUpload'
import {
  cancelProjectProblemPackageUpload,
  completeProjectProblemPackageUpload,
  createProjectProblemPackageUpload,
  getProjectProblemPackageUpload,
} from '@/generated/contracts'

vi.mock('@/generated/contracts', async (importOriginal) => ({
  ...(await importOriginal<typeof import('@/generated/contracts')>()),
  createProjectProblemPackageUpload: vi.fn(),
  completeProjectProblemPackageUpload: vi.fn(),
  getProjectProblemPackageUpload: vi.fn(),
  cancelProjectProblemPackageUpload: vi.fn(),
  getProjectProblemPackage: vi.fn(),
}))

/** Object-store PUTs keyed by URL: status per URL, or 'hold' to keep a PUT open until aborted. */
const putPlan = new Map<string, number | 'hold'>()
const putUrls: string[] = []
const aborted: string[] = []

class PlannedXhr {
  status = 0
  statusText = ''
  responseText = ''
  upload = { addEventListener: () => {} }
  private readonly listeners = new Map<string, () => void>()
  private url = ''
  open(_method: string, url: string) { this.url = url }
  setRequestHeader() {}
  addEventListener(type: string, listener: () => void) { this.listeners.set(type, listener) }
  abort() {
    aborted.push(this.url)
    this.listeners.get('abort')?.()
  }
  send() {
    putUrls.push(this.url)
    const planned = putPlan.get(this.url) ?? 200
    if (planned === 'hold') return
    queueMicrotask(() => {
      this.status = planned
      this.listeners.get('load')?.()
    })
  }
}

const file = (name: string, content = name) => new File([content], name, { type: 'text/plain' })
const pkg = { id: 'package-1', projectId: 'project-1', revision: 1, files: [], retention: {}, completedAt: '2026-10-12T00:00:00Z' }

function session(overrides: Record<string, unknown> = {}) {
  return {
    id: 'upload-1',
    projectId: 'project-1',
    revision: 2,
    state: 'pending',
    expiresAt: '2026-10-12T02:00:00Z',
    files: [
      { path: 'a.txt', sizeBytes: 5, mediaType: 'text/plain' },
      { path: 'b.txt', sizeBytes: 5, mediaType: 'text/plain' },
    ],
    uploadTargets: [
      { path: 'a.txt', uploadUrl: 'https://s3/a', requiredHeaders: {} },
      { path: 'b.txt', uploadUrl: 'https://s3/b', requiredHeaders: {} },
    ],
    uploadedPaths: [],
    ...overrides,
  }
}

const ok = (data: unknown) => ({ data, error: undefined }) as never
const problem = (status: number, diagnosticCode: string) => ({ data: undefined, error: { status, diagnosticCode, detail: diagnosticCode, retryable: true } }) as never
const lost = () => ({ data: undefined, error: { message: 'Network Error' } }) as never

function makeUpload(scope = 'test-scope', policyRevision = ref<number | undefined>(1)) {
  const scopeRunner = effectScope()
  const upload = scopeRunner.run(() => useProjectProblemPackageUpload(ref('project-1'), policyRevision, ref(null), ref('finite'), { recoveryScope: scope }))!
  return { upload, stop: () => scopeRunner.stop() }
}

function manyFilesSession(count: number) {
  const names = Array.from({ length: count }, (_, index) => `f${index}.txt`)
  return {
    names,
    session: session({
      files: names.map((path) => ({ path, sizeBytes: path.length, mediaType: 'text/plain' })),
      uploadTargets: names.map((path) => ({ path, uploadUrl: `https://s3/${path}`, requiredHeaders: {} })),
    }),
  }
}

function headersOf(mockFn: unknown, call = 0) {
  return (vi.mocked(mockFn as never) as unknown as { mock: { calls: [{ headers: Record<string, string> }][] } }).mock.calls[call][0].headers
}

describe('problem package upload recovery (#197)', () => {
  beforeEach(() => {
    vi.resetAllMocks()
    vi.stubGlobal('XMLHttpRequest', PlannedXhr)
    window.sessionStorage.clear()
    putPlan.clear()
    putUrls.length = 0
    aborted.length = 0
  })
  afterEach(() => vi.unstubAllGlobals())

  it('replays a lost create with the same key and never mints a second intent on 409', async () => {
    const { upload, stop } = makeUpload()
    upload.addFiles([file('a.txt'), file('b.txt')])
    vi.mocked(createProjectProblemPackageUpload)
      .mockResolvedValueOnce(lost())
      .mockResolvedValueOnce(problem(409, 'LW_IDEMPOTENCY_CONFLICT'))
      .mockResolvedValueOnce(ok(session()))
    vi.mocked(completeProjectProblemPackageUpload).mockResolvedValue(ok(pkg))

    await upload.createSession()
    expect(upload.state.kind).toBe('error')
    await upload.retry()
    expect(upload.state.kind).toBe('error')
    await upload.retry()
    await flushPromises()

    const keys = [0, 1, 2].map((call) => headersOf(createProjectProblemPackageUpload, call)['Idempotency-Key'])
    expect(new Set(keys).size).toBe(1)
    expect(upload.state.kind).toBe('done')
    stop()
  })

  it('queries the session after a failed PUT and only sends the missing file', async () => {
    const { upload, stop } = makeUpload()
    upload.addFiles([file('a.txt'), file('b.txt')])
    putPlan.set('https://s3/b', 500)
    vi.mocked(createProjectProblemPackageUpload).mockResolvedValue(ok(session()))
    await upload.createSession()
    await flushPromises()
    expect(upload.state.kind).toBe('error')
    expect(completeProjectProblemPackageUpload).not.toHaveBeenCalled()

    putPlan.set('https://s3/b', 200)
    putUrls.length = 0
    vi.mocked(getProjectProblemPackageUpload).mockResolvedValue(ok(session({ uploadedPaths: ['a.txt'] })))
    vi.mocked(completeProjectProblemPackageUpload).mockResolvedValue(ok(pkg))
    await upload.retry()
    await flushPromises()

    expect(getProjectProblemPackageUpload).toHaveBeenCalledWith({ path: { projectId: 'project-1', uploadId: 'upload-1' } })
    expect(putUrls).toEqual(['https://s3/b'])
    expect(upload.state.kind).toBe('done')
    stop()
  })

  it('a 412 from a PUT asks the session instead of overwriting', async () => {
    const { upload, stop } = makeUpload()
    upload.addFiles([file('a.txt'), file('b.txt')])
    putPlan.set('https://s3/a', 412)
    vi.mocked(createProjectProblemPackageUpload).mockResolvedValue(ok(session()))
    vi.mocked(getProjectProblemPackageUpload).mockResolvedValue(ok(session({ uploadedPaths: ['a.txt', 'b.txt'] })))
    vi.mocked(completeProjectProblemPackageUpload).mockResolvedValue(ok(pkg))
    await upload.createSession()
    await flushPromises()
    expect(getProjectProblemPackageUpload).toHaveBeenCalledTimes(1)
    expect(putUrls.filter((url) => url === 'https://s3/a')).toHaveLength(1)
    expect(upload.state.kind).toBe('done')
    stop()
  })

  it('keeps the first completion key and If-Match, and reads the session when the response is lost', async () => {
    const { upload, stop } = makeUpload()
    upload.addFiles([file('a.txt'), file('b.txt')])
    vi.mocked(createProjectProblemPackageUpload).mockResolvedValue(ok(session()))
    vi.mocked(completeProjectProblemPackageUpload).mockResolvedValueOnce(lost())
    vi.mocked(getProjectProblemPackageUpload).mockResolvedValue(ok(session({ state: 'completed', completedPackage: pkg, uploadedPaths: ['a.txt', 'b.txt'] })))
    await upload.createSession()
    await flushPromises()
    expect(completeProjectProblemPackageUpload).toHaveBeenCalledTimes(1)
    expect(headersOf(completeProjectProblemPackageUpload)['If-Match']).toBe('"rev-2"')
    expect(upload.state).toEqual({ kind: 'done', package: pkg })
    stop()
  })

  it('recovers a completing upload with the server key and revision', async () => {
    const { upload, stop } = makeUpload()
    upload.addFiles([file('a.txt'), file('b.txt')])
    vi.mocked(createProjectProblemPackageUpload).mockResolvedValue(ok(session()))
    vi.mocked(completeProjectProblemPackageUpload).mockResolvedValueOnce(problem(503, 'LW_STORE_UNAVAILABLE'))
    await upload.createSession()
    await flushPromises()
    expect(upload.state.kind).toBe('error')

    vi.mocked(getProjectProblemPackageUpload).mockResolvedValue(ok(session({ state: 'completing', revision: 3, completionIdempotencyKey: 'server-complete-key', completionExpectedRevision: 2, uploadedPaths: ['a.txt', 'b.txt'] })))
    vi.mocked(completeProjectProblemPackageUpload).mockResolvedValueOnce(ok(pkg))
    await upload.retry()
    await flushPromises()
    const headers = headersOf(completeProjectProblemPackageUpload, 1)
    expect(headers).toEqual({ 'Idempotency-Key': 'server-complete-key', 'If-Match': '"rev-2"' })
    expect(upload.state.kind).toBe('done')
    stop()
  })

  it('resumes after a refresh: asks for the same files and sends only the missing one', async () => {
    const first = makeUpload()
    first.upload.addFiles([file('a.txt'), file('b.txt')])
    putPlan.set('https://s3/b', 'hold')
    vi.mocked(createProjectProblemPackageUpload).mockResolvedValue(ok(session()))
    void first.upload.createSession()
    await flushPromises()
    first.stop() // page refresh: in-memory state is gone, the stored intent stays

    vi.mocked(getProjectProblemPackageUpload).mockResolvedValue(ok(session({ uploadedPaths: ['a.txt'] })))
    const second = makeUpload()
    await flushPromises()
    expect(second.upload.state).toEqual({ kind: 'needs-files', missing: ['b.txt'] })

    putPlan.set('https://s3/b', 200)
    putUrls.length = 0
    vi.mocked(completeProjectProblemPackageUpload).mockResolvedValue(ok(pkg))
    second.upload.addFiles([file('a.txt'), file('b.txt')])
    await second.upload.createSession()
    await flushPromises()
    expect(createProjectProblemPackageUpload).toHaveBeenCalledTimes(1)
    expect(putUrls).toEqual(['https://s3/b'])
    expect(second.upload.state.kind).toBe('done')
    expect(window.sessionStorage.getItem('labweaver:problem-package-upload:test-scope:project-1')).toBeNull()
    second.stop()
  })

  it('never revives an expired upload; the next start creates a new intent', async () => {
    const { upload, stop } = makeUpload()
    upload.addFiles([file('a.txt'), file('b.txt')])
    putPlan.set('https://s3/b', 500)
    vi.mocked(createProjectProblemPackageUpload).mockResolvedValue(ok(session()))
    await upload.createSession()
    await flushPromises()

    vi.mocked(getProjectProblemPackageUpload).mockResolvedValue(ok(session({ state: 'expired', terminalDiagnostic: 'LW_PACKAGE_UPLOAD_EXPIRED' })))
    await upload.retry()
    await flushPromises()
    expect(upload.state.kind === 'error' && upload.state.diagnostic.code).toBe('LW_PACKAGE_UPLOAD_EXPIRED')
    expect(completeProjectProblemPackageUpload).not.toHaveBeenCalled()

    vi.mocked(createProjectProblemPackageUpload).mockResolvedValue(ok(session({ id: 'upload-2' })))
    putPlan.set('https://s3/b', 200)
    vi.mocked(completeProjectProblemPackageUpload).mockResolvedValue(ok(pkg))
    await upload.createSession()
    await flushPromises()
    const firstKey = headersOf(createProjectProblemPackageUpload, 0)['Idempotency-Key']
    const secondKey = headersOf(createProjectProblemPackageUpload, 1)['Idempotency-Key']
    expect(secondKey).not.toBe(firstKey)
    stop()
  })

  it('cancels with the latest If-Match and a stable key, and stops in-flight PUTs', async () => {
    const { upload, stop } = makeUpload()
    upload.addFiles([file('a.txt'), file('b.txt')])
    putPlan.set('https://s3/b', 'hold')
    vi.mocked(createProjectProblemPackageUpload).mockResolvedValue(ok(session()))
    void upload.createSession()
    await flushPromises()
    expect(upload.state.kind).toBe('uploading')

    vi.mocked(getProjectProblemPackageUpload).mockResolvedValue(ok(session({ revision: 5, uploadedPaths: ['a.txt'] })))
    vi.mocked(cancelProjectProblemPackageUpload)
      .mockResolvedValueOnce(problem(503, 'LW_CONTROL_UNAVAILABLE'))
      .mockResolvedValueOnce(ok(session({ state: 'cancelled', revision: 6 })))
    await upload.cancel()
    expect(aborted).toEqual(['https://s3/b'])
    expect(upload.state.kind).toBe('error')
    await upload.cancel()
    await flushPromises()

    const first = headersOf(cancelProjectProblemPackageUpload, 0)
    const second = headersOf(cancelProjectProblemPackageUpload, 1)
    expect(first['If-Match']).toBe('"rev-5"')
    expect(second['Idempotency-Key']).toBe(first['Idempotency-Key'])
    expect(upload.state.kind).toBe('cancelled')
    expect(completeProjectProblemPackageUpload).not.toHaveBeenCalled()
    stop()
  })

  it('retry after a failed cancel continues the cancel and never resumes the upload', async () => {
    const { upload, stop } = makeUpload()
    upload.addFiles([file('a.txt'), file('b.txt')])
    putPlan.set('https://s3/b', 'hold')
    vi.mocked(createProjectProblemPackageUpload).mockResolvedValue(ok(session()))
    void upload.createSession()
    await flushPromises()

    vi.mocked(getProjectProblemPackageUpload).mockResolvedValue(ok(session({ revision: 4, uploadedPaths: ['a.txt'] })))
    vi.mocked(cancelProjectProblemPackageUpload)
      .mockResolvedValueOnce(lost())
      .mockResolvedValueOnce(ok(session({ state: 'cancelled', revision: 5 })))
    await upload.cancel()
    expect(upload.state.kind).toBe('error')
    putUrls.length = 0

    await upload.retry() // the banner's generic retry
    await flushPromises()
    const calls = vi.mocked(cancelProjectProblemPackageUpload).mock.calls
    expect(calls).toHaveLength(2)
    expect(headersOf(cancelProjectProblemPackageUpload, 1)['Idempotency-Key']).toBe(headersOf(cancelProjectProblemPackageUpload, 0)['Idempotency-Key'])
    expect(putUrls).toEqual([])
    expect(completeProjectProblemPackageUpload).not.toHaveBeenCalled()
    expect(upload.state.kind).toBe('cancelled')
    stop()
  })

  it('a refresh after a failed cancel continues the cancel, not the upload', async () => {
    const first = makeUpload()
    first.upload.addFiles([file('a.txt'), file('b.txt')])
    putPlan.set('https://s3/b', 'hold')
    vi.mocked(createProjectProblemPackageUpload).mockResolvedValue(ok(session()))
    void first.upload.createSession()
    await flushPromises()
    vi.mocked(getProjectProblemPackageUpload).mockResolvedValue(ok(session({ revision: 4, uploadedPaths: ['a.txt'] })))
    vi.mocked(cancelProjectProblemPackageUpload).mockResolvedValueOnce(problem(503, 'LW_CONTROL_UNAVAILABLE'))
    await first.upload.cancel()
    const firstKey = headersOf(cancelProjectProblemPackageUpload, 0)['Idempotency-Key']
    first.stop()

    vi.mocked(cancelProjectProblemPackageUpload).mockResolvedValueOnce(ok(session({ state: 'cancelled', revision: 5 })))
    putUrls.length = 0
    const second = makeUpload()
    await flushPromises()
    expect(headersOf(cancelProjectProblemPackageUpload, 1)['Idempotency-Key']).toBe(firstKey)
    expect(second.upload.state.kind).toBe('cancelled')
    expect(second.upload.state.kind).not.toBe('needs-files')
    expect(putUrls).toEqual([])
    expect(completeProjectProblemPackageUpload).not.toHaveBeenCalled()
    second.stop()
  })

  it('sends at most three PUTs at a time and a cancel stops the queued files', async () => {
    const { upload, stop } = makeUpload()
    const { names, session: many } = manyFilesSession(10)
    for (const name of names) putPlan.set(`https://s3/${name}`, 'hold')
    upload.addFiles(names.map((name) => file(name)))
    vi.mocked(createProjectProblemPackageUpload).mockResolvedValue(ok(many))
    void upload.createSession()
    await flushPromises()
    expect(putUrls).toHaveLength(3)

    vi.mocked(getProjectProblemPackageUpload).mockResolvedValue(ok({ ...many, revision: 3 }))
    vi.mocked(cancelProjectProblemPackageUpload).mockResolvedValue(ok({ ...many, state: 'cancelled', revision: 4 }))
    await upload.cancel()
    await flushPromises()
    expect(aborted).toHaveLength(3)
    expect(putUrls).toHaveLength(3)
    expect(upload.state.kind).toBe('cancelled')
    stop()
  })

  it('replays the frozen create request after a lost response even if the policy revision changed', async () => {
    const revision = ref<number | undefined>(1)
    const { upload, stop } = makeUpload('test-scope', revision)
    upload.addFiles([file('a.txt'), file('b.txt')])
    vi.mocked(createProjectProblemPackageUpload).mockResolvedValueOnce(lost())
    await upload.createSession()
    revision.value = 2
    vi.mocked(createProjectProblemPackageUpload).mockResolvedValueOnce(ok(session()))
    vi.mocked(completeProjectProblemPackageUpload).mockResolvedValue(ok(pkg))
    await upload.retry()
    await flushPromises()
    const calls = vi.mocked(createProjectProblemPackageUpload).mock.calls as unknown as [{ headers: Record<string, string>; body: unknown }][]
    expect(calls).toHaveLength(2)
    expect(calls[1][0].headers['Idempotency-Key']).toBe(calls[0][0].headers['Idempotency-Key'])
    expect(calls[1][0].body).toEqual(calls[0][0].body)
    expect(calls[1][0].body).toMatchObject({ retentionPolicyRevision: 1 })
    expect(upload.state.kind).toBe('done')
    stop()
  })

  it('a refresh after a lost create replays the frozen request, then asks for the files', async () => {
    const first = makeUpload()
    first.upload.addFiles([file('a.txt'), file('b.txt')])
    vi.mocked(createProjectProblemPackageUpload).mockResolvedValueOnce(lost())
    await first.upload.createSession()
    first.stop()

    vi.mocked(createProjectProblemPackageUpload).mockResolvedValueOnce(ok(session()))
    const second = makeUpload('test-scope', ref(7))
    await flushPromises()
    const calls = vi.mocked(createProjectProblemPackageUpload).mock.calls as unknown as [{ headers: Record<string, string>; body: unknown }][]
    expect(calls[1][0].headers['Idempotency-Key']).toBe(calls[0][0].headers['Idempotency-Key'])
    expect(calls[1][0].body).toEqual(calls[0][0].body)
    expect(second.upload.state).toEqual({ kind: 'needs-files', missing: ['a.txt', 'b.txt'] })
    second.stop()
  })

  it('a generic retry after a lost create replays the frozen request even if course or retention changed', async () => {
    const course = ref<string | null>('course-a')
    const retention = ref<'finite' | 'course_archive'>('finite')
    const scopeRunner = effectScope()
    const upload = scopeRunner.run(() => useProjectProblemPackageUpload(ref('project-1'), ref(1), course as never, retention as never, { recoveryScope: 'test-scope' }))!
    upload.addFiles([file('a.txt'), file('b.txt')])
    vi.mocked(createProjectProblemPackageUpload).mockResolvedValueOnce(lost())
    await upload.createSession()
    course.value = 'course-b'
    retention.value = 'course_archive'
    vi.mocked(createProjectProblemPackageUpload).mockResolvedValueOnce(ok(session()))
    vi.mocked(completeProjectProblemPackageUpload).mockResolvedValue(ok(pkg))
    await upload.retry()
    await flushPromises()
    const calls = vi.mocked(createProjectProblemPackageUpload).mock.calls as unknown as [{ headers: Record<string, string>; body: Record<string, unknown> }][]
    expect(calls).toHaveLength(2)
    expect(calls[1][0].headers['Idempotency-Key']).toBe(calls[0][0].headers['Idempotency-Key'])
    expect(calls[1][0].body).toEqual(calls[0][0].body)
    expect(calls[1][0].body).toMatchObject({ courseId: 'course-a', retentionChoice: 'finite' })
    scopeRunner.stop()
  })
})
