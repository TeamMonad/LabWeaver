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

  it('takes its accessible name from a wrapping <label> without aria-label', () => {
    const value = ref<string | number | null | undefined>('')
    const wrapper = mount(defineComponent({
      setup: () => () => h('label', null, [
        h('span', null, '计费单位'),
        h(AppSelect, { modelValue: value.value }, () => [h('option', { value: '' }, '全部')]),
      ]),
    }), { attachTo: document.body })
    const trigger = wrapper.get('[role="combobox"]')
    // A <button> is labelable, so the enclosing <label> names it; the mirror stays hidden.
    expect(trigger.element.tagName).toBe('BUTTON')
    expect(trigger.element.closest('label')?.textContent).toContain('计费单位')
    expect(trigger.attributes('aria-hidden')).toBeUndefined()
    wrapper.unmount()
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

  it('names the visible field from a wrapping <label> caption when aria-label is absent', async () => {
    const wrapper = mount(defineComponent({
      setup: () => () => h('label', null, [
        h('span', null, '生效时间'),
        h(AppDateTimePicker, { modelValue: '2030-01-01T08:30' }),
      ]),
    }), { attachTo: document.body })
    await nextTick()
    const group = wrapper.get('[role="group"]')
    const labelledby = group.attributes('aria-labelledby')
    expect(labelledby).toBeTruthy()
    const names = labelledby!.split(' ').map((id) => document.getElementById(id)?.textContent?.trim())
    expect(names).toEqual(['生效时间'])
    expect(group.attributes('aria-label')).toBeUndefined()
    wrapper.unmount()
  })

  it('forwards id, aria-labelledby and aria-describedby to the visible field', async () => {
    const wrapper = mount(defineComponent({
      setup: () => () => h('div', null, [
        h('span', { id: 'expiry-label' }, '批准有效期'),
        h('span', { id: 'expiry-hint' }, '到期后需要重新申请'),
        h(AppDateTimePicker, { id: 'expiry', 'aria-labelledby': 'expiry-label', 'aria-describedby': 'expiry-hint' }),
      ]),
    }), { attachTo: document.body })
    await nextTick()
    const group = wrapper.get('[role="group"]')
    expect(group.attributes('id')).toBe('expiry')
    expect(group.attributes('aria-labelledby')).toBe('expiry-label')
    expect(group.attributes('aria-describedby')).toBe('expiry-hint')
    const native = wrapper.get('input[data-app-datetime]')
    expect(native.attributes('id')).toBeUndefined()
    expect(native.attributes('aria-hidden')).toBe('true')
    wrapper.unmount()
  })
})
