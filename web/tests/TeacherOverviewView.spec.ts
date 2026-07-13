import { describe, expect, it } from 'vitest'
import { mount } from '@vue/test-utils'
import TeacherOverviewView from '@/views/teacher/TeacherOverviewView.vue'

describe('TeacherOverviewView', () => {
  it('does not present business rows when fixture mode is disabled', () => {
    const wrapper = mount(TeacherOverviewView)

    expect(wrapper.text()).toContain('未绑定数据源，未展示实验条目。')
    expect(wrapper.find('button').attributes('disabled')).toBeDefined()
  })
})
