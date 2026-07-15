import { describe, it, expect, vi, beforeEach } from 'vitest'
import { mount } from '@vue/test-utils'
import { createPinia } from 'pinia'
import { ref } from 'vue'
import TopAppBar from '@/components/layout/TopAppBar.vue'

const loginMock = vi.fn()
const logoutMock = vi.fn()

vi.mock('@/composables/useAuth', () => ({
  useAuth: () => ({
    user: ref(null),
    isLoading: ref(false),
    error: ref(null),
    isAuthenticated: ref(false),
    login: loginMock,
    logout: logoutMock,
    handleCallback: vi.fn(),
    loadUser: vi.fn(),
  }),
}))

vi.mock('@/config', () => ({
  OIDC_ENABLED: true,
  API_BASE_URL: '/api/v1',
  APP_TITLE: 'LabWeaver',
}))

const createWrapper = () => {
  return mount(TopAppBar, {
    props: { drawerOpen: false },
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

describe('TopAppBar', () => {
  beforeEach(() => {
    vi.clearAllMocks()
  })

  it('shows unauthenticated state and login button', () => {
    const wrapper = createWrapper()
    expect(wrapper.text()).toContain('未认证')
    expect(wrapper.text()).toContain('登录')
  })

  it('emits toggleDrawer when menu button is clicked', async () => {
    const wrapper = createWrapper()
    await wrapper.find('button[aria-label="打开导航"]').trigger('click')
    expect(wrapper.emitted('toggleDrawer')).toHaveLength(1)
  })
})
