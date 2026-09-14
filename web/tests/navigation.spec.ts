import { describe, it, expect, vi } from 'vitest'
import { mount, flushPromises } from '@vue/test-utils'
import { createRouter, createMemoryHistory } from 'vue-router'
import GcpSearchBar from '@/components/layout/GcpSearchBar.vue'
import {
  NAVIGATION_GROUPS,
  consoleNavigationTarget,
  navigationTarget,
  navigationGroupsForRoles,
  normalizeRoles,
} from '@/utils/navigation'

const authState = vi.hoisted(() => ({
  user: { value: { expired: false, profile: { roles: ['admin'] } } },
  isAuthenticated: { value: true },
}))

const projectsState = vi.hoisted(() => ({ selectedProjectId: 'project-1' as string | null }))

vi.mock('@/composables/useAuth', () => ({
  useAuth: () => authState,
}))

vi.mock('@/composables/useProjects', () => ({
  useProjects: () => projectsState,
}))

describe('navigation rules', () => {
  it('normalizes only the platform role claim values', () => {
    expect(normalizeRoles(['teacher', 'platform_admin', 'teacher', 'researcher'])).toEqual(['teacher', 'admin'])
    expect(normalizeRoles('student,admin')).toEqual(['student', 'admin'])
    expect(normalizeRoles(['Teacher', 'platform-admin'])).toEqual([])
  })

  it('makes all groups available when the user has all platform roles', () => {
    const groups = navigationGroupsForRoles(['teacher', 'student', 'admin'])
    expect(groups.map((group) => group.id)).toEqual(['teaching', 'student', 'work', 'admin'])
    expect(groups.find((group) => group.id === 'work')?.items).toHaveLength(4)
  })

  it('keeps project context and global admin actions separate', () => {
    const workspaces = NAVIGATION_GROUPS.find((group) => group.id === 'work')?.items.find((item) => item.id === 'workspaces')
    const approval = NAVIGATION_GROUPS.find((group) => group.id === 'admin')?.items.find((item) => item.id === 'admin-resource-approval')
    const finance = NAVIGATION_GROUPS.find((group) => group.id === 'admin')?.items.find((item) => item.id === 'admin-finance')
    expect(workspaces && navigationTarget(workspaces, 'project with spaces')).toBe('/researcher/workspaces?projectId=project+with+spaces')
    expect(approval && navigationTarget(approval, 'project-1')).toBe('/admin/resource-approval')
    expect(finance && navigationTarget(finance, 'project-1')).toBe('/admin/resource-finance?projectId=project-1')
  })

  it('opens the student console only in a student context', () => {
    expect(consoleNavigationTarget(['student'], '/student/labs', 'project-1')).toBe('/student/environments?projectId=project-1')
    expect(consoleNavigationTarget(['teacher'], '/teacher/overview', 'project-1')).toBe('/researcher/environments?projectId=project-1')
    expect(consoleNavigationTarget(['student'], '/student/labs', 'project-1', 'env-123')).toBe('/student/environments?projectId=project-1&environmentId=env-123')
  })

  it('searches only authorized tasks and routes resource approval to its real page', async () => {
    authState.user.value = { expired: false, profile: { roles: ['admin'] } }
    authState.isAuthenticated.value = true
    projectsState.selectedProjectId = 'project-1'
    const router = createRouter({
      history: createMemoryHistory(),
      routes: [{ path: '/:pathMatch(.*)*', component: { template: '<div />' } }],
    })
    await router.push('/')
    await router.isReady()
    const wrapper = mount(GcpSearchBar, { global: { plugins: [router] } })

    const input = wrapper.find('input[aria-label="搜索任务或按环境 ID 直达"]')
    await input.setValue('资源审批')
    await input.trigger('focus')

    expect(wrapper.find('.search-dropdown').text()).toContain('资源审批')
    expect(wrapper.find('.item-desc').text()).toBe('平台管理 · 审核资源申请并管理资源使用授权。')
    expect(wrapper.find('.search-dropdown').text()).not.toContain('我的实验')

    await wrapper.find('.dropdown-item').trigger('click')
    await flushPromises()
    expect(router.currentRoute.value.path).toBe('/admin/resource-approval')
  })

  it('explains when a task search has no matches', async () => {
    const router = createRouter({
      history: createMemoryHistory(),
      routes: [{ path: '/:pathMatch(.*)*', component: { template: '<div />' } }],
    })
    await router.push('/')
    await router.isReady()
    const wrapper = mount(GcpSearchBar, { global: { plugins: [router] } })

    const input = wrapper.find('input[aria-label="搜索任务或按环境 ID 直达"]')
    await input.setValue('不存在的任务')
    await input.trigger('focus')

    expect(wrapper.find('.search-empty').text()).toBe('没有匹配的任务。')
  })
})
