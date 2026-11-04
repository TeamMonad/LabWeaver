<template>
  <section class="permission-card md-card" aria-labelledby="permission-title" data-testid="runtime-permissions">
    <h4 id="permission-title" class="permission-card__title">运行权限</h4>
    <template v-if="authorization">
      <p class="permission-card__note">以下是这个环境的资源批准中记录的运行权限；启动、重启和恢复前平台会重新核对，不能借重启扩大权限。</p>
      <dl class="permission-list">
        <div v-for="row in rows" :key="row.key" :data-permission="row.key" :data-elevated="row.elevated ? 'true' : 'false'">
          <dt>{{ row.label }}</dt>
          <dd>{{ row.value }}</dd>
        </div>
      </dl>
    </template>
    <p v-else class="permission-card__note" data-testid="runtime-permissions-missing">
      平台没有返回这个环境的运行授权记录（较早创建或尚未完成交接的环境没有此记录）。实际权限以平台运行时核对为准，本页不推测。
    </p>
    <DiagnosticBanner
      v-if="rejection"
      :diagnostic="rejection"
      severity="error"
      data-testid="runtime-permission-rejection"
    />
  </section>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import type { EnvironmentDetailSchema } from '@/generated/contracts'
import { useEnvironmentEntryContext } from '@/composables/useEnvironmentEntry'
import {
  effectivePermissionRows,
  knownEnvironmentCode,
  persistedEnvironmentDiagnostic,
  type DeclaredRunAsUser,
} from '@/utils/environmentRetention'

/**
 * Effective runtime permissions as the server reports them: the runtime
 * authorization receipt on the environment detail (root/UID, capabilities,
 * privileged and its execution boundary). A runtime refusal recorded on the
 * current operation is shown with its Chinese reason first, then the code.
 */
const props = defineProps<{ data: EnvironmentDetailSchema }>()

const { releases } = useEnvironmentEntryContext()

const authorization = computed(() => props.data.runtimeAuthorization ?? null)

/** Only manually published releases expose their reviewed spec; otherwise the declaration is unknown. */
const declaredRunAsUser = computed<DeclaredRunAsUser>(() => {
  const state = releases.releases
  if (state.kind !== 'success') return undefined
  const release = state.data.find((item) => item.id === props.data.releaseId && item.version === props.data.releaseVersion)
  const security = release?.manualApproval?.spec.security
  return security ? security.runAsUser ?? null : undefined
})

const rows = computed(() => authorization.value ? effectivePermissionRows(props.data, authorization.value, declaredRunAsUser.value) : [])

const rejection = computed(() => {
  const operationCode = props.data.operation.state === 'failed' ? props.data.operation.diagnosticCode : null
  const code = operationCode ?? props.data.lastDiagnosticCode ?? null
  return code && (code.startsWith('LW_RUNTIME_') || knownEnvironmentCode(code)) ? persistedEnvironmentDiagnostic(code) : null
})
</script>

<style scoped>
.permission-card { display: grid; gap: var(--space-3); padding: var(--space-4); border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-medium); }
.permission-card__title { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-small); }
.permission-card__note { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.permission-list { display: grid; gap: var(--space-2); margin: 0; }
.permission-list > div { display: grid; grid-template-columns: minmax(120px, 160px) minmax(0, 1fr); gap: var(--space-3); }
.permission-list dt { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-large); }
.permission-list dd { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); overflow-wrap: anywhere; }
.permission-list > div[data-elevated='true'] dd { font-weight: 500; }
</style>
