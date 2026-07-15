import { describe, it, expect, vi, beforeEach } from 'vitest'
import { mount } from '@vue/test-utils'
import { createPinia } from 'pinia'
import AuthErrorView from '@/views/AuthErrorView.vue'

const loginMock = vi.fn()

vi.mock('@/composables/useAuth', () => ({
  useAuth: () => ({
    login: loginMock,
  }),
}))

vi.mock('@/config', () => ({
  OIDC_ENABLED: true,
  OIDC_CONFIG: {},
  API_BASE_URL: '/api/v1',
  APP_TITLE: 'LabWeaver',
}))

function createWrapper(reason?: string) {
  return mount(AuthErrorView, {
    props: { reason },
    global: {
      plugins: [createPinia()],
      stubs: {
        RouterLink: {
          props: ['to'],
          template: '<a :href="to"><slot /></a>',
        },
      },
    },
  })
}

describe('AuthErrorView', () => {
  beforeEach(() => {
    vi.clearAllMocks()
  })

  it('renders role_denied diagnostics', () => {
    const wrapper = createWrapper('role_denied')
    expect(wrapper.text()).toContain('无权访问该角色页面')
    expect(wrapper.text()).toContain('当前账号没有访问该角色工作台的权限')
  })

  it('renders session_required diagnostics', () => {
    const wrapper = createWrapper('session_required')
    expect(wrapper.text()).toContain('需要登录')
    expect(wrapper.text()).toContain('该页面需要登录后才能访问')
  })

  it('renders oidc_not_configured diagnostics', () => {
    const wrapper = createWrapper('oidc_not_configured')
    expect(wrapper.text()).toContain('身份认证未配置')
  })

  it('calls login when the login button is clicked', async () => {
    const wrapper = createWrapper('session_required')
    await wrapper.find('button').trigger('click')
    expect(loginMock).toHaveBeenCalled()
  })
})
