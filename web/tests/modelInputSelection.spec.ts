import { describe, expect, it } from 'vitest'
import { mount } from '@vue/test-utils'
import { computed, defineComponent, nextTick, ref, type Ref } from 'vue'
import ModelInputPathPicker from '@/components/common/ModelInputPathPicker.vue'
import { useModelInputPathSelection, type ModelInputPackageLike } from '@/composables/useModelInputPathSelection'
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

const spacedOnlyFiles = [
  {
    path: ' README.md ',
    object: { artifactId: 'a1', mediaType: 'text/markdown', objectVersion: 'v1', sizeBytes: 12, storeBinding: 'materials' },
  },
]

const dualSpacedFiles = [
  {
    path: ' README.md ',
    object: { artifactId: 'a1', mediaType: 'text/markdown', objectVersion: 'v1', sizeBytes: 12, storeBinding: 'materials' },
  },
  {
    path: 'README.md',
    object: { artifactId: 'a2', mediaType: 'text/markdown', objectVersion: 'v1', sizeBytes: 34, storeBinding: 'materials' },
  },
]

const pkgRef: Ref<ModelInputPackageLike> = ref(null)

const SelectionHost = defineComponent({
  setup() {
    const selection = useModelInputPathSelection(computed(() => pkgRef.value))
    return { selection }
  },
  template: '<div />',
})

function mountSelection(pkg: ModelInputPackageLike) {
  pkgRef.value = pkg
  const wrapper = mount(SelectionHost)
  const selection = (wrapper.vm as unknown as { selection: ReturnType<typeof useModelInputPathSelection> }).selection
  return { wrapper, selection }
}

describe('modelInputSelection helpers', () => {
  it('preserves exact path identity without trimming and dedupes by exact string', () => {
    // Spaced and unspaced are distinct; whitespace-only is a legal relative path; only '' is dropped.
    expect(buildSelectedPaths([' README.md ', 'src/main.py', 'README.md', '', '  ', ' README.md '])).toEqual([
      ' README.md ',
      'src/main.py',
      'README.md',
      '  ',
    ])
    expect(hasRequiredSelectedPaths([])).toBe(false)
    expect(hasRequiredSelectedPaths(null)).toBe(false)
    expect(hasRequiredSelectedPaths([''])).toBe(false)
    expect(hasRequiredSelectedPaths(['  '])).toBe(true)
    expect(hasRequiredSelectedPaths([' README.md '])).toBe(true)
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
    const { wrapper, selection } = mountSelection({
      id: 'package-1',
      files: sampleFiles,
    })
    expect(selection.selectedPaths).toEqual([])
    expect(selection.hasSelection).toBe(false)

    selection.toggle('README.md')
    await nextTick()
    expect(selection.selectedPaths).toEqual(['README.md'])
    expect(selection.hasSelection).toBe(true)

    pkgRef.value = { id: 'package-2', files: sampleFiles }
    await nextTick()
    expect(selection.selectedPaths).toEqual([])
    expect(selection.hasSelection).toBe(false)
    wrapper.unmount()
  })

  it('keeps spaced-only manifest path exact through selectedPaths', async () => {
    const { wrapper, selection } = mountSelection({
      id: 'package-spaced',
      files: spacedOnlyFiles,
    })

    selection.toggle(' README.md ')
    await nextTick()
    expect(selection.selectedPaths).toEqual([' README.md '])
    expect(selection.hasSelection).toBe(true)
    // Allowed-path filter must not drop the spaced choice after buildSelectedPaths.
    selection.setSelected([' README.md '])
    await nextTick()
    expect(selection.selectedPaths).toEqual([' README.md '])
    wrapper.unmount()
  })

  it('choosing spaced name does not select the unspaced twin', async () => {
    const { wrapper, selection } = mountSelection({
      id: 'package-dual',
      files: dualSpacedFiles,
    })

    selection.toggle(' README.md ')
    await nextTick()
    expect(selection.selectedPaths).toEqual([' README.md '])
    expect(selection.isSelected(' README.md ')).toBe(true)
    expect(selection.isSelected('README.md')).toBe(false)

    selection.setSelected([' README.md '])
    await nextTick()
    expect(selection.selectedPaths).toEqual([' README.md '])
    expect(selection.isSelected('README.md')).toBe(false)
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

  it('emits exact spaced-only path end-to-end into selectedPaths', async () => {
    const wrapper = mount(ModelInputPathPicker, {
      props: {
        files: spacedOnlyFiles,
        modelValue: [],
      },
    })
    await wrapper.get('[data-testid="model-input-path- README.md "]').setValue(true)
    expect(wrapper.emitted('update:modelValue')?.at(-1)?.[0]).toEqual([' README.md '])
    await wrapper.setProps({ modelValue: [' README.md '] })
    expect(wrapper.get('[data-testid="model-input-selection-summary"]').text()).toContain('已选择 1 / 1')
    wrapper.unmount()
  })

  it('choosing spaced checkbox emits spaced path, not the unspaced twin', async () => {
    const wrapper = mount(ModelInputPathPicker, {
      props: {
        files: dualSpacedFiles,
        modelValue: [],
      },
    })
    await wrapper.get('[data-testid="model-input-path- README.md "]').setValue(true)
    const emitted = wrapper.emitted('update:modelValue')?.at(-1)?.[0]
    expect(emitted).toEqual([' README.md '])
    expect(emitted).not.toEqual(['README.md'])
    await wrapper.setProps({ modelValue: [' README.md '] })
    expect(
      (wrapper.get('[data-testid="model-input-path- README.md "]').element as HTMLInputElement).checked,
    ).toBe(true)
    expect(
      (wrapper.get('[data-testid="model-input-path-README.md"]').element as HTMLInputElement).checked,
    ).toBe(false)
    wrapper.unmount()
  })
})
