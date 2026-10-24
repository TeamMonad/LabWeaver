<template>
  <section
    class="upgrade-panel md-card"
    aria-labelledby="template-upgrade-heading"
    data-testid="template-upgrade"
    :data-runtime-kind="data.runtimeKind"
  >
    <h4 id="template-upgrade-heading" class="upgrade-panel__title">模板版本</h4>
    <p class="upgrade-panel__current">
      固定使用 <strong>{{ currentRelease ? workTemplateTitle(currentRelease) : `v${data.releaseVersion}` }}</strong>。模板发布新版本时，这个 Work 不会被自动替换。
      <span v-if="targetNotYetRunning" data-testid="upgrade-target-note">这是已确认的升级目标版本，升级操作成功后才表示新版本已在运行。</span>
    </p>

    <div
      v-if="upgradeOperation"
      class="upgrade-status"
      role="status"
      data-testid="upgrade-status"
      :data-state="upgradeOperation.state"
    >
      <strong>
        模板升级 v{{ upgradeOperation.templateUpgrade?.sourceReleaseVersion }} → v{{ upgradeOperation.templateUpgrade?.targetReleaseVersion }}：{{ environmentOperationStateLabel(upgradeOperation.state) }}
      </strong>
      <span v-if="upgradeActive">升级会重启 Work，完成前无法连接；/workspace 中的文件保留。页面会自动刷新状态。</span>
      <span v-else-if="upgradeOperation.state === 'succeeded'">升级已完成，Work 正在使用新版本。</span>
      <template v-else-if="upgradeOperation.state === 'failed'">
        <span>升级没有完成，Work 停在失败状态；/workspace 中的文件不会因此被删除。{{ operationFailureText }}</span>
        <span>可以使用顶部操作栏的“重试失败的操作”，平台仍升级到同一目标版本。</span>
      </template>
      <span v-else-if="upgradeOperation.state === 'cancelled'">升级已取消；取消会停止 Work，/workspace 保留。</span>
    </div>

    <p v-if="data.runtimeKind === 'virtual_machine'" class="upgrade-panel__note" data-testid="vm-upgrade-unsupported" role="note">
      虚拟机 Work 不支持在原 Work 内升级模板：需要导出/导入流程，当前不可用。如需使用新版本，请用新版本模板新建 Work，再自行把需要的数据迁移过去。
    </p>
    <p v-else class="upgrade-panel__note" data-testid="upgrade-unavailable" role="note">
      本版本暂不支持在原资金租约上升级模板。可以用新版本新建 Work，再自行迁移 /workspace 数据。
    </p>
    <p v-if="releaseProblem" class="upgrade-panel__note">{{ releaseProblem }}</p>
    <p v-else-if="!newestRelease" class="upgrade-panel__note">这个模板系列暂时没有更新的版本。</p>
    <RouterLink
      v-else
      class="outlined-button small"
      data-testid="new-work-link"
      :to="{ path: '/researcher/resources', query: { projectId: data.projectId, releaseId: newestRelease.id, releaseVersion: String(newestRelease.version) } }"
    >
      用 {{ workTemplateTitle(newestRelease) }} 新建 Work
    </RouterLink>
  </section>
</template>

<script setup lang="ts">
import { computed, watch } from 'vue'
import { RouterLink } from 'vue-router'
import type { EnvironmentDetailSchema } from '@/generated/contracts'
import { useAuth } from '@/composables/useAuth'
import { useEnvironmentEntryContext } from '@/composables/useEnvironmentEntry'
import { makeDiagnostic } from '@/types/async'
import { actorIdFromProfile } from '@/utils/navigation'
import { clearPendingCommandsWithScopePrefix } from '@/utils/pendingCommands'
import { environmentOperationStateLabel } from '@/utils/stateLabels'
import { newerWorkReleases, workTemplateTitle } from '@/utils/workTemplate'

/**
 * Template version of a Work. Every Work runs on its original funded lease and
 * this version refuses to switch its template in place
 * (`LW_WORK_TEMPLATE_UPGRADE_UNAVAILABLE`, Refs #288), so the panel offers a
 * new Work on the newest version of the same family instead. An upgrade the
 * server accepted earlier still shows its progress from the instance.
 */
const props = defineProps<{ data: EnvironmentDetailSchema }>()

const { releases } = useEnvironmentEntryContext()
const auth = useAuth()
const actorId = computed(() => actorIdFromProfile(auth.user.value?.profile) ?? null)

const releaseRows = computed(() => releases.releases.kind === 'success' ? releases.releases.data : [])
const currentRelease = computed(() => releaseRows.value.find((release) => (
  release.id === props.data.releaseId && release.version === props.data.releaseVersion
)) ?? null)
const releaseProblem = computed(() => {
  if (currentRelease.value) return null
  const state = releases.releases
  if (state.kind === 'idle' || state.kind === 'loading') return '正在读取模板版本…'
  if (state.kind === 'success' || state.kind === 'empty') return '当前模板版本已不在项目的模板列表中，暂时不能检查更新的版本。'
  return `无法读取模板列表：${state.diagnostic.message}`
})
const newestRelease = computed(() => currentRelease.value ? newerWorkReleases(releaseRows.value, currentRelease.value)[0] ?? null : null)

/**
 * Only the instance's current operation describes an upgrade: it is re-read
 * while it runs and cannot be overtaken by an older history read. Earlier
 * upgrades stay in the operation timeline.
 */
const upgradeOperation = computed(() => {
  const current = props.data.operation
  return current.templateUpgrade ? { state: current.state, templateUpgrade: current.templateUpgrade, diagnosticCode: current.diagnosticCode } : null
})
const upgradeActive = computed(() => {
  const state = upgradeOperation.value?.state
  return state === 'accepted' || state === 'running' || state === 'cancelling'
})
const operationFailureText = computed(() => {
  const code = upgradeOperation.value?.diagnosticCode
  if (!code) return ''
  const diagnostic = makeDiagnostic(code, '')
  return diagnostic.message ? `原因：${diagnostic.message}（${code}）` : `平台诊断码：${code}。`
})
/** While an upgrade has not succeeded, the Work's release fields name the confirmed target, not what runs. */
const targetNotYetRunning = computed(() => {
  const operation = upgradeOperation.value
  return Boolean(operation && operation.state !== 'succeeded'
    && operation.templateUpgrade?.targetReleaseId === props.data.releaseId
    && operation.templateUpgrade.targetReleaseVersion === props.data.releaseVersion)
})

// An earlier version of this page kept unconfirmed upgrade commands for a
// retry. No upgrade can be sent any more, so their keys are dropped instead
// of waiting in session storage for a retry that never comes.
watch(
  [actorId, () => props.data.id],
  ([actor, environmentId]) => {
    if (actor) clearPendingCommandsWithScopePrefix(actor, `work-template-upgrade:${environmentId}:`)
  },
  { immediate: true },
)
</script>

<style scoped>
.upgrade-panel { display: grid; justify-items: start; gap: var(--space-3); padding: var(--space-4); border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-medium); }
.upgrade-panel__title { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-small); }
.upgrade-panel__current { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); overflow-wrap: anywhere; }
.upgrade-panel__note { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.upgrade-status { display: grid; gap: var(--space-1); justify-self: stretch; padding: var(--space-3); border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container); color: var(--md-sys-color-on-surface); font: var(--md-sys-body-small); }
</style>
