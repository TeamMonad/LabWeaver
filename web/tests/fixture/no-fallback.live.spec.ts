import { describe, it, expect } from 'vitest'
import '@/api/client'
import { client as generatedClient } from '@/generated/contracts/client.gen'
import { listSshPublicKeys } from '@/generated/contracts'
import { DATA_MODE } from '@/config/dataMode'

const describeLive = DATA_MODE === 'live' ? describe : describe.skip

describeLive('live mode does not fall back to fixture', () => {
  it('fails with a real network error instead of fixture 401', async () => {
    await expect(
      listSshPublicKeys({ client: generatedClient, throwOnError: true }),
    ).rejects.toSatisfy((error: unknown) => {
      const axiosError = error as { response?: { status: number }; code?: string; message?: string; diagnosticCode?: string }
      // 任何 fixture 拦截都会产生 response.status=401；真实网络错误或 auth 失败没有 response。
      expect(axiosError.response).toBeUndefined()
      const transportFailure =
        axiosError.code === 'ECONNREFUSED' ||
        axiosError.code === 'ERR_NETWORK' ||
        /ECONNREFUSED|Network Error|fetch failed/i.test(axiosError.message ?? '') ||
        axiosError.diagnosticCode === 'LW_SDK_AUTH_TOKEN_UNAVAILABLE'
      expect(transportFailure).toBe(true)
      return true
    })
  })
})
