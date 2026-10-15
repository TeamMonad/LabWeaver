/* eslint-disable vue/one-component-per-file -- test harness hosts */
import { mount } from '@vue/test-utils'
import { defineComponent, h, nextTick, ref } from 'vue'
import { describe, expect, it } from 'vitest'
import AppSelect from '@/components/form/AppSelect.vue'
import AppDateTimePicker from '@/components/form/AppDateTimePicker.vue'

describe('AppSelect', () => {
  function mountSelect(initial: string | number) {
    const value = ref<string | number | null | undefined>(initial)
    const wrapper = mount(defineComponent({
      setup: () => () => h(AppSelect, {
        modelValue: value.value,
        'aria-label': '计费单位',
        'onUpdate:modelValue': (next: string | number | null | undefined) => { value.value = next },
      }, () => [
        h('option', { value: '' }, '全部'),
        h('option', { value: 2 }, '二'),
        h('option', { value: 'x', disabled: true }, '禁用'),
      ]),
    }), { attachTo: document.body })
    return { wrapper, value }
  }

  it('renders a Reka combobox trigger labelled like the native control', () => {
    const { wrapper } = mountSelect('')
    const trigger = wrapper.get('[role="combobox"]')
    expect(trigger.attributes('aria-label')).toBe('计费单位')
    expect(trigger.text()).toContain('全部')
  })

  it('keeps original option value types and supports the empty option', async () => {
    const { wrapper, value } = mountSelect('')
    await wrapper.get('select').setValue('2')
    expect(value.value).toBe(2)
    await wrapper.get('select').setValue('')
    expect(value.value).toBe('')
    wrapper.unmount()
  })

  it('hides the native mirror from assistive tech and tab order', () => {
    const { wrapper } = mountSelect('')
    const native = wrapper.get('select')
    expect(native.attributes('aria-hidden')).toBe('true')
    expect(native.attributes('tabindex')).toBe('-1')
  })
})

describe('AppDateTimePicker', () => {
  it('round-trips datetime-local strings and exposes a labelled field', async () => {
    const value = ref('2030-01-01T08:30')
    const wrapper = mount(defineComponent({
      setup: () => () => h(AppDateTimePicker, {
        modelValue: value.value,
        'aria-label': '生效时间',
        'onUpdate:modelValue': (next: string) => { value.value = next },
      }),
    }), { attachTo: document.body })
    await nextTick()
    expect(wrapper.find('[role="group"][aria-label="生效时间"]').exists() || wrapper.find('[aria-label="生效时间"]').exists()).toBe(true)
    await wrapper.get('input[data-app-datetime]').setValue('2031-02-03T04:05')
    expect(value.value).toBe('2031-02-03T04:05')
    wrapper.unmount()
  })
})
