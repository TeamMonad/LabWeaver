import { describe, expect, it } from 'vitest'
import { mount } from '@vue/test-utils'
import { computed, defineComponent, nextTick, ref } from 'vue'
import ModelInputPathPicker from '@/components/common/ModelInputPathPicker.vue'
import { useModelInputPathSelection } from '@/composables/useModelInputPathSelection'
import {
  buildSelectedPaths,
  hasRequiredSelectedPaths,
  modelInputSelectionAction,
  modelInputSelectionMessage,
} from '@/utils/modelInputSelection'

const sampleFiles = [
  {
    path: 'README.md',
    object: { artifactId: 'a1', mediaType: 'text/markdown', objectVersion: 'v1', sizeBytes: 12, storeBinding: 'materials' },
  },
  {
    path: 'src/main.py',
    object: { artifactId: 'a2', mediaType: 'text/x-python', objectVersion: 'v1', sizeBytes: 34, storeBinding: 'materials' },
  },
]

describe('modelInputSelection helpers', () => {
  it('builds non-empty exact paths without duplicates or blank entries', () => {
    expect(buildSelectedPaths([' README.md ', 'src/main.py', 'README.md', '', '  '])).toEqual([
      'README.md',
      'src/main.py',
    ])
    expect(hasRequiredSelectedPaths([])).toBe(false)
    expect(hasRequiredSelectedPaths(null)).toBe(false)
    expect(hasRequiredSelectedPaths(['README.md'])).toBe(true)
  })

  it('maps selection and durable authorization diagnostics', () => {
    expect(modelInputSelectionAction('LW_MODEL_INPUT_SELECTION_REQUIRED')).toContain('显式勾选')
    expect(modelInputSelectionAction('LW_MODEL_INPUT_AUTHORIZATION_DENIED')).toContain('授权复核')
    expect(modelInputSelectionAction('LW_MODEL_INPUT_AUTHORIZATION_UNAVAILABLE')).toContain('复核连接')
    expect(modelInputSelectionMessage('LW_MODEL_INPUT_SELECTION_REQUIRED', 'fallback')).toContain('必须显式选择')
  })
})

describe('useModelInputPathSelection', () => {
  it('never auto-selects the whole package and resets on package id change', async () => {
    const pkg = ref<{ id: string; files: typeof sampleFiles } | null>({
      id: 'package-1',
      files: sampleFiles,
    })
    const Host = defineComponent({
      setup() {
        const selection = useModelInputPathSelection(computed(() => pkg.value))
        return { selection }
      },
      template: '<div />',
    })
    const wrapper = mount(Host)
    const selection = (wrapper.vm as unknown as { selection: ReturnType<typeof useModelInputPathSelection> }).selection
    expect(selection.selectedPaths).toEqual([])
    expect(selection.hasSelection).toBe(false)

    selection.toggle('README.md')
    await nextTick()
    expect(selection.selectedPaths).toEqual(['README.md'])
    expect(selection.hasSelection).toBe(true)

    pkg.value = { id: 'package-2', files: sampleFiles }
    await nextTick()
    expect(selection.selectedPaths).toEqual([])
    expect(selection.hasSelection).toBe(false)
    wrapper.unmount()
  })
})

describe('ModelInputPathPicker', () => {
  it('requires an explicit checkbox choice and emits selectedPaths shape', async () => {
    const wrapper = mount(ModelInputPathPicker, {
      props: {
        files: sampleFiles,
        modelValue: [],
      },
    })
    expect(wrapper.get('[data-testid="model-input-selection-summary"]').text()).toContain('尚未选择')
    await wrapper.get('[data-testid="model-input-path-README.md"]').setValue(true)
    expect(wrapper.emitted('update:modelValue')?.at(-1)?.[0]).toEqual(['README.md'])
    await wrapper.setProps({ modelValue: ['README.md'] })
    expect(wrapper.get('[data-testid="model-input-selection-summary"]').text()).toContain('已选择 1 / 2')
    await wrapper.get('[data-testid="model-input-clear-selection"]').trigger('click')
    expect(wrapper.emitted('update:modelValue')?.at(-1)?.[0]).toEqual([])
    wrapper.unmount()
  })
})
