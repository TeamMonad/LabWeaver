import { describe, expect, it } from 'vitest'
import { mount } from '@vue/test-utils'
import TeacherOverviewView from '@/views/teacher/TeacherOverviewView.vue'

describe('TeacherOverviewView', () => {
  it('does not present business rows before API binding', () => {
    const wrapper = mount(TeacherOverviewView)

    expect(wrapper.text()).toContain('未绑定数据源，未展示实验条目。')
    expect(wrapper.text()).toContain('等待课程 API 绑定')
  })
})
