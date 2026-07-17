import { describe, expect, it, vi } from 'vitest'
import { mount } from '@vue/test-utils'
import TeacherWorkbenchShell from '@/components/teacher/TeacherWorkbenchShell.vue'

vi.mock('vue-router', async (importOriginal) => {
  const actual = await importOriginal<typeof import('vue-router')>()
  return { ...actual, useRoute: () => ({ meta: { title: '实验总览' } }) }
})

describe('TeacherWorkbenchShell', () => {
  it('provides the six workbench modules and an explicit API-not-bound diagnostic', () => {
    const wrapper = mount(TeacherWorkbenchShell, {
      global: {
        stubs: {
          RouterLink: { props: ['to'], template: '<a :href="to"><slot /></a>' },
          RouterView: { template: '<div />' },
        },
      },
    })

    expect(wrapper.findAll('.module-nav a')).toHaveLength(6)
    expect(wrapper.text()).toContain('材料')
    expect(wrapper.text()).toContain('课程与实验 API 尚未绑定')
  })

  it('explains that the create action cannot create data before API binding', async () => {
    const wrapper = mount(TeacherWorkbenchShell, {
      global: {
        stubs: {
          RouterLink: { template: '<a><slot /></a>' },
          RouterView: { template: '<div />' },
        },
      },
    })

    await wrapper.get('.primary-action').trigger('click')
    expect(wrapper.text()).toContain('此入口不会创建任何业务数据。')
  })
})
