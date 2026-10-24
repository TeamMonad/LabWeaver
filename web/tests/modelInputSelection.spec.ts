import { describe, expect, it } from 'vitest'
import { mount } from '@vue/test-utils'
import { nextTick, ref } from 'vue'
import ModelInputPathPicker from '@/components/common/ModelInputPathPicker.vue'
import { useModelInputSelection } from '@/composables/useModelInputSelection'
import type { ProblemPackageSchema } from '@/generated/contracts'
import { buildSelectedPaths } from '@/utils/modelInputSelection'

function packageFile(path: string): ProblemPackageSchema['files'][number] {
  return {
    path,
    sha256: 'a'.repeat(64),
    object: { artifactId: `artifact-${path}`, mediaType: 'text/plain', objectVersion: 'v1', sizeBytes: 2048, storeBinding: 'materials' },
  }
}

function packageVersion(id: string, revision: number, paths: string[]): ProblemPackageSchema {
  return {
    id,
    projectId: 'project-1',
    courseId: null,
    revision,
    files: paths.map(packageFile),
    retention: { disposition: 'retain_until_revoked', retainUntil: null },
    completedAt: '2026-10-24T00:00:00.000Z',
  } as unknown as ProblemPackageSchema
}

describe('buildSelectedPaths', () => {
  it('keeps exact path strings, including surrounding spaces, in package order', () => {
    const packagePaths = ['a.md', ' a.md', 'a.md ', 'dir/b.py']
    expect(buildSelectedPaths(['dir/b.py', 'a.md ', ' a.md'], packagePaths)).toEqual([' a.md', 'a.md ', 'dir/b.py'])
  })

  it('drops duplicates and paths that are not in the package', () => {
    expect(buildSelectedPaths(['a.md', 'a.md', 'other.md', ''], ['a.md', 'b.md'])).toEqual(['a.md'])
  })

  it('never falls back to the whole package for an empty choice', () => {
    expect(buildSelectedPaths([], ['a.md', 'b.md'])).toEqual([])
  })
})

describe('useModelInputSelection', () => {
  it('starts empty and clears the choice when the package version changes', async () => {
    const pkg = ref<ProblemPackageSchema | null>(packageVersion('package-1', 1, ['a.md', 'b.md']))
    const selection = useModelInputSelection(pkg)
    expect(selection.selectedPaths.value).toEqual([])

    selection.checked.value = ['b.md']
    expect(selection.selectedPaths.value).toEqual(['b.md'])

    pkg.value = packageVersion('package-1', 2, ['a.md', 'b.md'])
    await nextTick()
    expect(selection.selectedPaths.value).toEqual([])

    selection.checked.value = ['a.md']
    pkg.value = packageVersion('package-2', 1, ['a.md'])
    await nextTick()
    expect(selection.selectedPaths.value).toEqual([])
  })
})

describe('ModelInputPathPicker', () => {
  const files = [packageFile('lab.md'), packageFile(' spaced.md'), packageFile('tests/check.py')]

  it('renders every file unchecked and explains why starting is unavailable', () => {
    const wrapper = mount(ModelInputPathPicker, { props: { files, modelValue: [] } })
    const options = wrapper.findAll('[data-testid="model-input-path-option"]')
    expect(options.map((option) => (option.element as HTMLInputElement).value)).toEqual(['lab.md', ' spaced.md', 'tests/check.py'])
    expect(options.every((option) => !(option.element as HTMLInputElement).checked)).toBe(true)
    expect(wrapper.get('[data-testid="model-input-summary"]').text()).toContain('请至少勾选一个要交给模型的文件')
  })

  it('emits the exact selection in package order', async () => {
    const wrapper = mount(ModelInputPathPicker, { props: { files, modelValue: ['tests/check.py'] } })
    await wrapper.findAll('[data-testid="model-input-path-option"]')[1].setValue(true)
    expect(wrapper.emitted('update:modelValue')?.at(-1)).toEqual([[' spaced.md', 'tests/check.py']])
  })

  it('selects every file only on an explicit action and can clear the choice', async () => {
    const wrapper = mount(ModelInputPathPicker, { props: { files, modelValue: ['lab.md'] } })
    await wrapper.get('[data-testid="model-input-select-all"]').trigger('click')
    expect(wrapper.emitted('update:modelValue')?.at(-1)).toEqual([['lab.md', ' spaced.md', 'tests/check.py']])
    await wrapper.get('[data-testid="model-input-clear"]').trigger('click')
    expect(wrapper.emitted('update:modelValue')?.at(-1)).toEqual([[]])
  })
})
