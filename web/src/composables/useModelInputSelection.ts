import { computed, ref, watch, type Ref } from 'vue'
import type { ProblemPackageSchema } from '@/generated/contracts'
import { buildSelectedPaths } from '@/utils/modelInputSelection'

/**
 * Files the user explicitly chose to send to the model for one package version (#216).
 * Every new package id or revision starts with nothing selected.
 */
export function useModelInputSelection(pkg: Ref<ProblemPackageSchema | null>) {
  const checked = ref<string[]>([])

  watch(
    () => (pkg.value ? `${pkg.value.id}:${pkg.value.revision}` : null),
    () => {
      checked.value = []
    },
  )

  const selectedPaths = computed(() => buildSelectedPaths(checked.value, pkg.value?.files.map((file) => file.path) ?? []))

  return { checked, selectedPaths }
}
