import { computed, reactive, ref, watch, type Ref, type ComputedRef } from 'vue'
import { buildSelectedPaths, hasRequiredSelectedPaths } from '@/utils/modelInputSelection'

export type ModelInputPackageFile = {
  path: string
  object?: { sizeBytes?: number }
}

export type ModelInputPackageLike = {
  id: string
  files: readonly ModelInputPackageFile[]
} | null | undefined

/**
 * Explicit multi-file selection for model input (#216).
 * Selection starts empty for every package id; never auto-selects the whole package.
 */
export function useModelInputPathSelection(pkg: Ref<ModelInputPackageLike> | ComputedRef<ModelInputPackageLike>) {
  const selected = ref<string[]>([])
  let boundPackageId: string | null = null

  function resetForPackage(next: ModelInputPackageLike) {
    if (!next) {
      boundPackageId = null
      selected.value = []
      return
    }
    if (boundPackageId !== next.id) {
      boundPackageId = next.id
      selected.value = []
      return
    }
    const allowed = new Set(next.files.map((file) => file.path))
    selected.value = selected.value.filter((path) => allowed.has(path))
  }

  watch(
    pkg,
    (next) => {
      resetForPackage(next)
    },
    { immediate: true },
  )

  const availableFiles = computed(() => pkg.value?.files ?? [])
  const selectedPaths = computed(() => buildSelectedPaths(selected.value))
  const hasSelection = computed(() => hasRequiredSelectedPaths(selected.value))
  const selectedCount = computed(() => selectedPaths.value.length)

  function isSelected(path: string): boolean {
    return selected.value.includes(path)
  }

  function setSelected(paths: string[]) {
    const allowed = new Set(availableFiles.value.map((file) => file.path))
    selected.value = buildSelectedPaths(paths).filter((path) => allowed.has(path))
  }

  function toggle(path: string) {
    if (!availableFiles.value.some((file) => file.path === path)) return
    if (isSelected(path)) {
      selected.value = selected.value.filter((item) => item !== path)
    } else {
      selected.value = [...selected.value, path]
    }
  }

  function clear() {
    selected.value = []
  }

  // reactive() auto-unwraps nested refs in both script and template access
  return reactive({
    availableFiles,
    selectedPaths,
    hasSelection,
    selectedCount,
    isSelected,
    setSelected,
    toggle,
    clear,
  })
}
