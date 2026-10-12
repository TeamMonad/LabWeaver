import { mount, flushPromises } from '@vue/test-utils'
import { afterEach, describe, expect, it } from 'vitest'
import { defineComponent, h, ref } from 'vue'
import ConfirmDialog from '@/components/common/ConfirmDialog.vue'

function host(dismissible = true) {
  const events: string[] = []
  const open = ref(false)
  const Host = defineComponent({
    setup: () => () => h('div', [
      h('button', { id: 'opener', onClick: () => { open.value = true } }, '删除'),
      h(ConfirmDialog, {
        open: open.value,
        title: '删除环境',
        description: '操作不可恢复。',
        confirmText: '删除',
        severity: 'error',
        dismissible,
        onConfirm: () => { events.push('confirm'); open.value = false },
        onCancel: () => { events.push('cancel'); open.value = false },
      }),
    ]),
  })
  const wrapper = mount(Host, { attachTo: document.body })
  return { wrapper, events, open }
}

const dialog = () => document.body.querySelector<HTMLElement>('[role="alertdialog"]')

async function openFromButton(wrapper: ReturnType<typeof host>['wrapper']) {
  const opener = wrapper.get('#opener').element as HTMLButtonElement
  opener.focus()
  opener.click()
  await flushPromises()
  await new Promise((resolve) => setTimeout(resolve, 0))
  return opener
}

describe('ConfirmDialog (Reka UI AlertDialog)', () => {
  afterEach(() => { document.body.innerHTML = '' })

  it('is an accessible alertdialog with title and description, focused on Cancel', async () => {
    const { wrapper } = host()
    await openFromButton(wrapper)
    const el = dialog()!
    expect(el).not.toBeNull()
    const title = document.getElementById(el.getAttribute('aria-labelledby')!)
    const desc = document.getElementById(el.getAttribute('aria-describedby')!)
    expect(title?.textContent).toBe('删除环境')
    expect(desc?.textContent).toBe('操作不可恢复。')
    expect(document.activeElement?.textContent?.trim()).toBe('取消')
    wrapper.unmount()
  })

  it('Esc cancels and focus returns to the opening button', async () => {
    const { wrapper, events } = host()
    const opener = await openFromButton(wrapper)
    dialog()!.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))
    await flushPromises()
    await new Promise((resolve) => setTimeout(resolve, 0))
    expect(events).toEqual(['cancel'])
    expect(dialog()).toBeNull()
    expect(document.activeElement).toBe(opener)
    wrapper.unmount()
  })

  it('confirm emits only confirm', async () => {
    const { wrapper, events } = host()
    await openFromButton(wrapper)
    document.body.querySelector<HTMLButtonElement>('[role="alertdialog"] .filled-button')!.click()
    await flushPromises()
    expect(events).toEqual(['confirm'])
    wrapper.unmount()
  })

  it('a non-dismissible dialog ignores Esc, has no Cancel and focuses the action', async () => {
    const { wrapper, events } = host(false)
    await openFromButton(wrapper)
    expect(document.body.querySelector('[role="alertdialog"] .text-button')).toBeNull()
    expect(document.activeElement?.textContent?.trim()).toBe('删除')
    dialog()!.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))
    await flushPromises()
    expect(events).toEqual([])
    expect(dialog()).not.toBeNull()
    wrapper.unmount()
  })
})
