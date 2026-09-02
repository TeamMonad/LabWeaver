import { vi } from 'vitest'

if (typeof globalThis.crypto === 'undefined') {
  Object.defineProperty(globalThis, 'crypto', { value: {} as Crypto })
}
if (!globalThis.crypto.subtle) {
  Object.defineProperty(globalThis.crypto, 'subtle', {
    value: {
      digest: vi.fn(async (_algorithm: string, buffer: ArrayBuffer | Uint8Array) => {
        const bytes = buffer instanceof Uint8Array ? buffer : new Uint8Array(buffer)
        const digest = new Uint8Array(32)
        for (let i = 0; i < bytes.length; i++) {
          digest[i % 32] ^= bytes[i]
        }
        return digest.buffer
      }),
    },
  })
}

Object.defineProperty(window, 'matchMedia', {
  writable: true,
  value: vi.fn().mockImplementation((query: string) => ({
    matches: query === '(prefers-color-scheme: dark)',
    media: query,
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
    dispatchEvent: vi.fn(),
  })),
})

const store: Record<string, string> = {}

Object.defineProperty(window, 'localStorage', {
  writable: true,
  value: {
    getItem: vi.fn((key: string) => store[key] ?? null),
    setItem: vi.fn((key: string, value: string) => {
      store[key] = value
    }),
    removeItem: vi.fn((key: string) => {
      delete store[key]
    }),
    clear: vi.fn(() => {
      for (const key of Object.keys(store)) {
        delete store[key]
      }
    }),
  },
})
