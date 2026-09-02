import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { mount } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import SshKeysView from '@/views/student/SshKeysView.vue'
import { listSshPublicKeys, createSshPublicKey } from '@/generated/contracts'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    listSshPublicKeys: vi.fn(),
    createSshPublicKey: vi.fn(),
    deleteSshPublicKey: vi.fn(),
  }
})

const mockKey = {
  id: 'key-1',
  actorId: 'actor-1',
  algorithm: 'ed25519',
  fingerprintSha256: 'a'.repeat(64),
  createdAt: '2026-07-11T10:00:00.000Z',
  revision: 1,
}

describe('SshKeysView', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    vi.resetAllMocks()
  })

  afterEach(() => {
    vi.unstubAllEnvs()
  })

  it('renders empty state when no SSH keys exist', async () => {
    vi.mocked(listSshPublicKeys).mockResolvedValue({ data: { items: [] }, error: undefined as never })
    const wrapper = mount(SshKeysView)
    await vi.waitFor(() => expect(wrapper.text()).toContain('暂无数据'))
    expect(wrapper.text()).toContain('SSH 公钥')
  })

  it('lists SSH keys and shows fingerprint', async () => {
    vi.mocked(listSshPublicKeys).mockResolvedValue({ data: { items: [mockKey] }, error: undefined as never })
    const wrapper = mount(SshKeysView)
    await vi.waitFor(() => expect(wrapper.text()).toContain('ed25519'))
    expect(wrapper.text()).toContain(mockKey.fingerprintSha256.slice(0, 8))
  })

  it('creates a new SSH key and refreshes the list', async () => {
    vi.mocked(listSshPublicKeys).mockResolvedValue({ data: { items: [] }, error: undefined as never })
    vi.mocked(createSshPublicKey).mockResolvedValue({ data: mockKey, error: undefined as never })
    const wrapper = mount(SshKeysView)
    await vi.waitFor(() => expect(wrapper.text()).toContain('暂无数据'))

    const input = wrapper.find('textarea')
    await input.setValue('ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExample user@host')
    await wrapper.find('button[type="button"].filled-button').trigger('click')

    await vi.waitFor(() => expect(vi.mocked(createSshPublicKey)).toHaveBeenCalledWith(
      expect.objectContaining({
        body: { publicKeyOpenssh: 'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExample user@host' },
      }),
    ))
  })
})
