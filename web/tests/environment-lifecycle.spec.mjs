import { describe, expect, it, vi } from 'vitest'

vi.mock('@playwright/test', () => {
  const ANY = Symbol('ANY')

  function matches(actual, expected) {
    if (expected === ANY) return actual !== undefined && actual !== null
    if (expected && typeof expected === 'object') {
      return actual && Object.entries(expected).every(([key, value]) => matches(actual[key], value))
    }
    return Object.is(actual, expected)
  }

  function fakeExpect(actual) {
    return {
      toBe(expected) {
        if (!Object.is(actual, expected)) throw new Error(`EXPECTED_${String(expected)}_GOT_${String(actual)}`)
      },
      toMatchObject(expected) {
        if (!matches(actual, expected)) throw new Error('EXPECTED_OBJECT_MATCH')
      },
      toBeVisible() {},
      toBeEnabled() {},
    }
  }
  fakeExpect.any = () => ANY
  fakeExpect.poll = (callback) => ({
    async toBe(expected) {
      let actual
      for (let attempt = 0; attempt < 4; attempt += 1) {
        actual = await callback()
        if (Object.is(actual, expected)) return
      }
      throw new Error(`POLL_EXPECTED_${String(expected)}_GOT_${String(actual)}`)
    },
  })
  return { expect: fakeExpect }
})

const { deleteEnvironmentByUi } = await import('../e2e/support/environment-lifecycle.mjs')

function response(body, status = 200) {
  return {
    ok: () => status >= 200 && status < 300,
    status: () => status,
    text: async () => JSON.stringify(body),
  }
}

function fakePage(environmentStates) {
  let environmentRead = 0
  let mutationResponse = 0
  const mutations = []
  const locator = {
    click: async () => {},
    getByRole: () => locator,
  }
  return {
    mutations,
    request: {
      get: async (path) => {
        if (path === '/api/v1/environments/environment') {
          const current = environmentStates[Math.min(environmentRead++, environmentStates.length - 1)]
          return current === null ? response({}, 404) : response(current)
        }
        if (path.endsWith('/cancel-operation')) return response({ state: 'cancelled' })
        if (path.endsWith('/delete-operation')) return response({ state: 'succeeded' })
        throw new Error(`UNEXPECTED_GET:${path}`)
      },
    },
    goto: async () => {},
    getByRole: () => locator,
    waitForResponse: async () => {
      mutationResponse += 1
      if (mutationResponse === 1) {
        mutations.push('cancel')
        return response({
          environmentId: 'environment',
          operationId: 'cancel-operation',
          statusUrl: '/api/v1/environments/environment/operations/cancel-operation',
        })
      }
      mutations.push('delete')
      return response({
        environmentId: 'environment',
        operationId: 'delete-operation',
        statusUrl: '/api/v1/environments/environment/operations/delete-operation',
      })
    },
  }
}

describe('environment lifecycle cleanup', () => {
  it('accepts a cancelled operation after physical deletion and does not submit a duplicate delete', async () => {
    const page = fakePage([
      { observedState: 'provisioning', desiredState: 'running' },
      { observedState: 'deleted', desiredState: 'deleted' },
      null,
    ])
    await deleteEnvironmentByUi(page, {
      projectId: 'project',
      environmentId: 'environment',
    })
    expect(page.mutations).toEqual(['cancel'])
  })

  it('waits for a cancelled operation to settle before submitting normal delete', async () => {
    const page = fakePage([
      { observedState: 'provisioning', desiredState: 'running' },
      { observedState: 'failed', desiredState: 'running' },
      null,
    ])
    await deleteEnvironmentByUi(page, {
      projectId: 'project',
      environmentId: 'environment',
    })
    expect(page.mutations).toEqual(['cancel', 'delete'])
  })
})
