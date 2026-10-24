<template>
  <section
    class="upgrade-panel md-card"
    aria-labelledby="template-upgrade-heading"
    data-testid="template-upgrade"
    :data-runtime-kind="data.runtimeKind"
  >
    <h4 id="template-upgrade-heading" class="upgrade-panel__title">模板版本</h4>
    <p class="upgrade-panel__current">
      <template v-if="targetNotYetRunning">
        升级目标版本 <strong>{{ currentRelease ? workTemplateTitle(currentRelease) : `v${data.releaseVersion}` }}</strong>。
        <span data-testid="upgrade-target-note">这是已确认的目标版本，升级操作成功后才表示新版本已在运行。</span>
      </template>
      <template v-else>
        固定使用 <strong>{{ currentRelease ? workTemplateTitle(currentRelease) : `v${data.releaseVersion}` }}</strong>。模板发布新版本时，这个 Work 不会被自动替换。
      </template>
    </p>

    <div
      v-if="upgradeOperation"
      class="upgrade-status"
      role="status"
      data-testid="upgrade-status"
      :data-state="upgradeOperation.state"
    >
      <strong>
        模板升级 v{{ upgradeOperation.templateUpgrade.sourceReleaseVersion }} → 目标版本 v{{ upgradeOperation.templateUpgrade.targetReleaseVersion }}：{{ environmentOperationStateLabel(upgradeOperation.state) }}
      </strong>
      <span v-if="upgradeActive">升级会重启 Work，完成前无法连接；/workspace 中的文件保留。页面会自动刷新状态。</span>
      <span v-else-if="upgradeOperation.state === 'succeeded'">升级已完成，Work 正在使用 v{{ upgradeOperation.templateUpgrade.targetReleaseVersion }}。</span>
      <template v-else-if="upgradeOperation.state === 'failed'">
        <span>升级没有完成，新版本没有运行；/workspace 中的文件不会因此被删除。{{ operationFailureText }}</span>
        <span>可以使用顶部操作栏的“重试失败的操作”，平台仍升级到同一目标版本，不会另开资源租约。</span>
      </template>
      <span v-else-if="upgradeOperation.state === 'cancelled'">升级已取消；取消会停止 Work，/workspace 保留。</span>
    </div>

    <template v-if="data.runtimeKind === 'virtual_machine'">
      <p class="upgrade-panel__note" data-testid="vm-upgrade-unsupported" role="note">
        虚拟机 Work 不支持在原 Work 内升级模板：需要导出/导入流程，当前不可用。如需使用新版本，请用新版本模板新建 Work，再自行把需要的数据迁移过去。
      </p>
      <RouterLink
        v-if="candidates[0]"
        class="outlined-button small"
        data-testid="vm-new-work-link"
        :to="resourceRequestLink(candidates[0])"
      >
        用 {{ workTemplateTitle(candidates[0]) }} 新建 Work
      </RouterLink>
    </template>

    <template v-else-if="!upgradeActive">
      <p v-if="releaseProblem" class="upgrade-panel__note">{{ releaseProblem }}</p>
      <p v-else-if="candidates.length === 0" class="upgrade-panel__note">这个模板系列暂时没有更新的版本。</p>
      <ul v-else class="upgrade-list" aria-label="可升级的版本">
        <li
          v-for="release in candidates"
          :key="`${release.id}:${release.version}`"
          class="upgrade-option"
          :data-release-id="release.id"
          :data-release-version="release.version"
        >
          <span class="upgrade-option__text">
            <strong>{{ workTemplateTitle(release) }}</strong>
            <span>发布于 {{ formatTimestamp(release.publishedAt) }}</span>
            <span v-if="release.description">{{ release.description }}</span>
          </span>
          <button
            type="button"
            class="outlined-button small"
            :disabled="!canUpgrade"
            :title="upgradeBlockReason ?? undefined"
            :data-testid="`upgrade-to-${release.version}`"
            @click="pendingTarget = release"
          >
            {{ submitting ? '提交中…' : `升级到 v${release.version}` }}
          </button>
        </li>
      </ul>
      <p v-if="candidates.length > 0 && upgradeBlockReason" class="upgrade-panel__note" role="note">{{ upgradeBlockReason }}</p>
    </template>

    <div v-if="upgradeDiagnostic" class="upgrade-result">
      <DiagnosticBanner :diagnostic="upgradeDiagnostic" @retry="retryUpgrade" />
      <p v-if="!finalRefusal" class="upgrade-panel__note" role="status" data-testid="upgrade-unconfirmed">
        升级结果待确认：如果平台已经受理之前的请求，再次提交会沿用同一请求，不会重复升级。
      </p>
      <RouterLink
        v-if="upgradeDiagnostic.code === 'LW_RUNTIME_LEASE_NOT_ACTIVE'"
        class="outlined-button small"
        data-testid="lease-new-request-link"
        :to="currentReleaseRequestLink"
      >
        提交新的资源申请
      </RouterLink>
      <RouterLink
        v-else-if="newRequestForTarget"
        class="outlined-button small"
        data-testid="upgrade-new-request-link"
        :to="lastTargetRelease ? resourceRequestLink(lastTargetRelease) : currentReleaseRequestLink"
      >
        {{ lastTargetRelease ? `为 ${workTemplateTitle(lastTargetRelease)} 提交新的资源申请` : '提交新的资源申请' }}
      </RouterLink>
      <button
        v-else-if="reselectTarget"
        type="button"
        class="outlined-button small"
        data-testid="upgrade-refresh-releases"
        :disabled="releases.releases.kind === 'loading'"
        @click="refreshReleases"
      >
        刷新版本列表
      </button>
    </div>

    <ConfirmDialog
      :open="pendingTarget !== null"
      title="升级模板并重启 Work"
      :description="confirmText"
      confirm-text="升级并重启"
      severity="warning"
      @cancel="pendingTarget = null"
      @confirm="confirmUpgrade"
    />
  </section>
</template>

<script setup lang="ts">
import { computed, ref } from 'vue'
import { RouterLink } from 'vue-router'
import ConfirmDialog from '@/components/common/ConfirmDialog.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import { upgradeEnvironmentTemplate } from '@/generated/contracts'
import type { EnvironmentDetailSchema, EnvironmentTemplateReleaseViewSchema } from '@/generated/contracts'
import { useAuth } from '@/composables/useAuth'
import { useEnvironmentEntryContext } from '@/composables/useEnvironmentEntry'
import { diagnosticFromError, makeDiagnostic, type DiagnosticViewModel } from '@/types/async'
import { formatTimestamp, idempotencyKey, ifMatch } from '@/utils/format'
import { isTerminalEnvironment } from '@/utils/environmentLifecycle'
import { actorIdFromProfile } from '@/utils/navigation'
import { clearPendingCommand, readPendingCommand, writePendingCommand } from '@/utils/pendingCommands'
import { environmentOperationStateLabel } from '@/utils/stateLabels'
import { newerWorkReleases, workTemplateTitle } from '@/utils/workTemplate'

/**
 * Explicit Work template upgrade. Only container Work can switch to a newer
 * compatible version of its template family inside the same Work, on its
 * original resource approval and lease: the restart keeps /workspace and
 * discards everything else in the container. Progress comes only from the
 * instance's current operation. Virtual-machine Work is never offered an
 * in-place upgrade.
 */
const props = defineProps<{ data: EnvironmentDetailSchema }>()

const { releases, operations, env, permissions, hasActiveLifecycleOperation } = useEnvironmentEntryContext()
const auth = useAuth()
const actorId = computed(() => actorIdFromProfile(auth.user.value?.profile) ?? null)

type Release = EnvironmentTemplateReleaseViewSchema

const pendingTarget = ref<Release | null>(null)
const submitting = ref(false)
const upgradeDiagnostic = ref<DiagnosticViewModel | null>(null)
const lastTarget = ref<{ releaseId: string; version: number } | null>(null)

const releaseRows = computed(() => releases.releases.kind === 'success' ? releases.releases.data : [])
/** The release the last refused upgrade targeted; offered as the version for a new resource request. */
const lastTargetRelease = computed(() => {
  const target = lastTarget.value
  return target ? releaseRows.value.find((release) => release.id === target.releaseId && release.version === target.version) ?? null : null
})
const currentRelease = computed(() => releaseRows.value.find((release) => (
  release.id === props.data.releaseId && release.version === props.data.releaseVersion
)) ?? null)
const releaseProblem = computed(() => {
  if (currentRelease.value) return null
  const state = releases.releases
  if (state.kind === 'idle' || state.kind === 'loading') return '正在读取模板版本…'
  if (state.kind === 'success' || state.kind === 'empty') return '当前模板版本已不在项目的模板列表中，暂时不能检查可升级版本。'
  return `无法读取模板列表：${state.diagnostic.message}`
})
const candidates = computed(() => currentRelease.value ? newerWorkReleases(releaseRows.value, currentRelease.value) : [])

/**
 * Only the instance's current operation describes the upgrade: it is re-read
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
/**
 * Once an upgrade is accepted the Work's release fields name the confirmed
 * target; the new instance only runs it after the operation succeeded.
 */
const targetNotYetRunning = computed(() => {
  const operation = upgradeOperation.value
  return Boolean(operation && operation.state !== 'succeeded'
    && operation.templateUpgrade.targetReleaseId === props.data.releaseId
    && operation.templateUpgrade.targetReleaseVersion === props.data.releaseVersion)
})

const upgradeBlockReason = computed(() => {
  const denied = permissions.denialReason('start', '升级模板')
  if (denied) return denied
  if (isTerminalEnvironment(props.data) || props.data.desiredState === 'deleted') return '这个 Work 正在删除或已删除，不能升级模板。'
  if (hasActiveLifecycleOperation(props.data)) return '当前有操作正在进行，完成后才能升级模板。'
  if (!actorId.value) return '正在确认登录身份，确认后才能升级模板。'
  return null
})
const canUpgrade = computed(() => !submitting.value && upgradeBlockReason.value === null)

/** The server refused this exact target for good: resubmitting it is never offered as the way forward. */
const newRequestForTarget = computed(() => {
  const code = upgradeDiagnostic.value?.code
  return code === 'LW_WORK_TEMPLATE_UPGRADE_REQUIRES_RESOURCE_APPROVAL' || code === 'LW_RUNTIME_AUTHORIZATION_REAPPROVAL_REQUIRED'
})
const reselectTarget = computed(() => {
  const code = upgradeDiagnostic.value?.code
  return code === 'LW_WORK_TEMPLATE_UPGRADE_FAMILY_MISMATCH' || code === 'LW_WORK_TEMPLATE_UPGRADE_RELEASE_UNAVAILABLE'
})
const finalRefusal = computed(() => newRequestForTarget.value || reselectTarget.value)

const confirmText = computed(() => {
  const target = pendingTarget.value
  if (!target) return ''
  return `升级到 ${workTemplateTitle(target)} 会重启“${props.data.displayLabel}”：当前终端、桌面和 Web 应用连接会断开，正在运行的程序会结束。`
    + '/workspace 中的文件会保留；容器中 /workspace 以外的改动（例如另行安装的软件、临时文件）会丢失。'
    + '升级沿用这个 Work 原有的资源批准和使用授权，不会另开租约。升级受理后可在本页查看进度，升级成功前不算完成。确定升级吗？'
})

function resourceRequestLink(release: { id: string; version: number }) {
  return { path: '/researcher/resources', query: { projectId: props.data.projectId, releaseId: release.id, releaseVersion: String(release.version) } }
}
const currentReleaseRequestLink = computed(() => resourceRequestLink({ id: props.data.releaseId, version: props.data.releaseVersion }))

/**
 * One Idempotency-Key per (actor, environment, revision, target), kept as a
 * pending command in session storage: a retry or a page refresh before the
 * server answered replays the same command instead of submitting a second
 * upgrade. Another actor signing in on this browser never sees it. The key is
 * dropped only when this exact command is accepted: Environment checks
 * compatibility, permissions and the lease before its idempotency record can
 * answer a replay, so no refusal proves the first attempt was not accepted.
 */
function upgradeCommand(actor: string, releaseId: string, version: number) {
  const scope = `work-template-upgrade:${props.data.id}:${props.data.revision}:${releaseId}:${version}`
  const stored = readPendingCommand<null>(actor, scope)
  const command = stored?.fingerprint === scope ? stored : { fingerprint: scope, idempotencyKey: idempotencyKey(), data: null }
  writePendingCommand(actor, scope, command)
  return { scope, key: command.idempotencyKey }
}

async function submitUpgrade(releaseId: string, version: number) {
  const actor = actorId.value
  if (!canUpgrade.value || !actor) return
  const environmentId = props.data.id
  // A late answer for another environment or actor only updates storage, never this panel.
  const current = () => props.data.id === environmentId && actorId.value === actor
  submitting.value = true
  upgradeDiagnostic.value = null
  lastTarget.value = { releaseId, version }
  const { scope, key } = upgradeCommand(actor, releaseId, version)
  try {
    const result = await upgradeEnvironmentTemplate({
      path: { environmentId },
      headers: { 'Idempotency-Key': key, 'If-Match': ifMatch(props.data.revision) },
      body: { targetReleaseId: releaseId, targetReleaseVersion: version, confirmed: true },
    })
    if (result.error) {
      if (!current()) return
      upgradeDiagnostic.value = diagnosticFromError(result.error, 'WORK_TEMPLATE_UPGRADE_FAILED', '模板升级没有被受理。')
      if (upgradeDiagnostic.value.status === 403) void permissions.refresh()
      return
    }
    clearPendingCommand(actor, scope, key)
    if (!current()) return
    lastTarget.value = null
    await Promise.all([env.load(), operations.load()])
  } catch (error) {
    if (current()) upgradeDiagnostic.value = diagnosticFromError(error, 'WORK_TEMPLATE_UPGRADE_FAILED', '升级请求的结果尚未确认。')
  } finally {
    submitting.value = false
  }
}

async function confirmUpgrade() {
  const target = pendingTarget.value
  pendingTarget.value = null
  if (target) await submitUpgrade(target.id, target.version)
}

async function retryUpgrade() {
  const target = lastTarget.value
  if (target) await submitUpgrade(target.releaseId, target.version)
}

/** The refused target is not offered again until the user picks from a fresh list. */
async function refreshReleases() {
  upgradeDiagnostic.value = null
  lastTarget.value = null
  await releases.load()
}
</script>

<style scoped>
.upgrade-panel { display: grid; gap: var(--space-3); padding: var(--space-4); border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-medium); }
.upgrade-panel__title { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-small); }
.upgrade-panel__current { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); overflow-wrap: anywhere; }
.upgrade-panel__note { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.upgrade-status { display: grid; gap: var(--space-1); padding: var(--space-3); border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container); color: var(--md-sys-color-on-surface); font: var(--md-sys-body-small); }
.upgrade-list { display: grid; gap: var(--space-2); margin: 0; padding: 0; list-style: none; }
.upgrade-option { display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: var(--space-2) var(--space-4); padding: var(--space-2) 0; border-top: 1px solid var(--md-sys-color-outline-variant); }
.upgrade-option__text { display: grid; gap: 2px; min-width: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); overflow-wrap: anywhere; }
.upgrade-option__text strong { color: var(--md-sys-color-on-surface); font: var(--md-sys-title-small); }
.upgrade-result { display: grid; justify-items: start; gap: var(--space-2); }
</style>
