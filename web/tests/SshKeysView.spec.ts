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
  fingerprintSha256: `SHA256:${'A'.repeat(43)}`,
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
    await vi.waitFor(() => expect(wrapper.text()).toContain('还没有公钥？查看生成与添加指引'))
    expect(wrapper.text()).toContain('SSH 公钥')
    expect(wrapper.get('.ssh-key-guide summary').text()).toBe('还没有公钥？查看生成与添加指引')
    expect((wrapper.get('.ssh-key-guide').element as HTMLDetailsElement).open).toBe(false)
    expect(wrapper.get('.ssh-key-guide').text()).toContain('ssh-keygen -t ed25519')
    expect(wrapper.get('.ssh-key-guide').text()).toContain('cat /path/to/your-key.pub')
    expect(wrapper.get('.ssh-key-guide').text()).toContain('切勿粘贴或提交私钥')
  })

  it('lists SSH keys and shows fingerprint', async () => {
    vi.mocked(listSshPublicKeys).mockResolvedValue({ data: { items: [mockKey] }, error: undefined as never })
    const wrapper = mount(SshKeysView)
    await vi.waitFor(() => expect(wrapper.text()).toContain('ed25519'))
    expect(wrapper.text()).toContain(mockKey.fingerprintSha256.slice(0, 8))
    const fingerprintCode = wrapper.get(`code[title="${mockKey.fingerprintSha256}"]`)
    expect(fingerprintCode.attributes('title')).toBe(mockKey.fingerprintSha256)
    expect(fingerprintCode.text()).toBe('SHA256:A…AAAAAAAA')
  })

  it('creates a new SSH key and refreshes the list', async () => {
    vi.mocked(listSshPublicKeys).mockResolvedValue({ data: { items: [] }, error: undefined as never })
    vi.mocked(createSshPublicKey).mockResolvedValue({ data: mockKey, error: undefined as never })
    const wrapper = mount(SshKeysView)
    await vi.waitFor(() => expect(wrapper.text()).toContain('还没有公钥？查看生成与添加指引'))

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
