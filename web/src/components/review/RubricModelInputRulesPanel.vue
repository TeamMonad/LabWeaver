<template>
  <section class="rubric-rules" aria-labelledby="rubric-rules-heading" data-testid="rubric-model-input-rules">
    <h4 id="rubric-rules-heading">评分规则交给模型的设置</h4>
    <p class="rubric-note">
      AI 评语步骤读取的评分规则默认隐藏，不会交给模型。设为“课程内公开”只改变可见性；还需要单独批准交给当前模型，评语步骤才会读取它。修改可见性会清除已有的模型批准。
    </p>

    <AsyncStateView :state="rules.state" empty-text="当前没有可维护的评分规则。" @retry="rules.load">
      <template #success>
        <p v-if="editDeniedReason" class="rubric-note" role="status" data-testid="rubric-edit-denied">
          {{ editDeniedReason }}
        </p>
        <ul class="rubric-list">
          <li v-for="row in rows" :key="row.path" class="rubric-row" data-testid="rubric-rule-row">
            <div class="rubric-heading">
              <code class="rubric-path">{{ row.path }}</code>
              <span class="rubric-steps">评语步骤：{{ row.stepIds.join('、') }}</span>
            </div>
            <p v-if="!row.identity" class="rubric-status rubric-status--blocked" role="status">
              材料包中没有这个路径的文件，无法设置；该评分规则不会交给模型。
            </p>
            <template v-else>
              <p class="rubric-status" :class="{ 'rubric-status--allowed': rowAllowsModel(row) }" role="status" data-testid="rubric-rule-status">
                {{ rowStatus(row) }}
              </p>
              <div v-if="canEdit" class="rubric-actions">
                <button
                  v-if="rowVisibility(row) === 'hidden'"
                  type="button"
                  class="outlined-button"
                  :disabled="busy"
                  data-testid="rubric-set-public"
                  @click="submit(row, { kind: 'set_visibility', visibility: 'course_public' })"
                >
                  设为课程内公开
                </button>
                <button
                  v-else
                  type="button"
                  class="outlined-button"
                  :disabled="busy"
                  data-testid="rubric-set-hidden"
                  @click="requestHidden(row)"
                >
                  设为隐藏
                </button>
                <button
                  v-if="rowVisibility(row) === 'course_public' && !rowAllowsModel(row)"
                  type="button"
                  class="filled-button"
                  :disabled="busy || !syncedBinding"
                  data-testid="rubric-approve-model"
                  @click="approve(row)"
                >
                  批准交给模型 {{ syncedBinding?.model ?? '' }}
                </button>
                <button
                  v-if="row.rule?.approval"
                  type="button"
                  class="text-button"
                  :disabled="busy"
                  data-testid="rubric-revoke-model"
                  @click="pending = { row, action: { kind: 'revoke_model_use' } }"
                >
                  撤回模型批准
                </button>
              </div>
            </template>
          </li>
        </ul>
        <p v-if="canEdit && rowsNeedPolicy && !syncedBinding" class="rubric-note" role="status" data-testid="rubric-policy-unsynced">
          正在核对项目 AI 设置与评分规则是否属于同一策略版本，核对完成前不能批准交给模型。
        </p>
      </template>
    </AsyncStateView>

    <DiagnosticBanner
      v-if="rules.outcome"
      :diagnostic="rules.outcome"
      :severity="rules.outcome.code.endsWith('_ACCEPTED') ? 'info' : 'error'"
    />

    <ConfirmDialog
      :open="pending !== null"
      :title="pending?.action.kind === 'revoke_model_use' ? '撤回模型批准？' : '设为隐藏并清除模型批准？'"
      :description="pendingDescription"
      :confirm-text="pending?.action.kind === 'revoke_model_use' ? '撤回批准' : '设为隐藏'"
      cancel-text="取消"
      severity="warning"
      @cancel="pending = null"
      @confirm="confirmPending"
    />
  </section>
</template>

<script setup lang="ts">
import { computed, ref, toRef, watch } from 'vue'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import ConfirmDialog from '@/components/common/ConfirmDialog.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import { useActiveProjectLlmPolicy } from '@/composables/useActiveProjectLlmPolicy'
import { useProjectAvailableActions } from '@/composables/useProjectAvailableActions'
import { useRubricModelInputRules } from '@/composables/useRubricModelInputRules'
import type {
  MaintainRubricModelInputRuleRequestSchema,
  ProblemPackageSchema,
  RubricModelInputRule,
  RubricModelInputRuleAction,
  RubricModelInputVisibility,
} from '@/generated/contracts'

type RubricIdentity = MaintainRubricModelInputRuleRequestSchema['rubric']

/** One rubric file referenced by advisory steps of the published Evaluation release. */
interface RubricRow {
  path: string
  stepIds: string[]
  identity: RubricIdentity | null
  rule: RubricModelInputRule | null
}

/** A destructive change waiting for the teacher's confirmation. */
interface PendingChange {
  row: RubricRow
  action: RubricModelInputRuleAction
}

const props = defineProps<{
  projectId: string
  evaluationReleaseId: string
  evaluationReleaseRevision: number
  problemPackage: ProblemPackageSchema
  advisorySteps: ReadonlyArray<{ id: string; rubric: string }>
}>()

const projectIdRef = toRef(props, 'projectId')
const rules = useRubricModelInputRules(projectIdRef)
const policy = useActiveProjectLlmPolicy(projectIdRef)
// Reading the rules needs Browse; every change needs Edit. Unknown, expired or unreadable permission offers no change.
const permissions = useProjectAvailableActions(projectIdRef)
const canEdit = computed(() => permissions.allows('edit'))
const editDeniedReason = computed(() => permissions.denialReason('edit', '修改评分规则的模型使用设置'))
const pending = ref<PendingChange | null>(null)
const busy = computed(() => rules.acting || policy.state.kind === 'loading')

/**
 * The model an approval targets. It is only trusted when the AI settings and the rules were read from the
 * same active policy version; otherwise another change may have replaced the model in between.
 */
const syncedBinding = computed(() => {
  if (policy.state.kind !== 'success' || rules.state.kind !== 'success') return null
  const active = policy.state.data
  const current = rules.state.data
  return active.id === current.policyId && active.revision === current.policyRevision ? active.binding : null
})

// The two reads can land on different policy versions (a save creates a new one; another tab may change the
// model). Re-read whichever side is strictly older, once per distinct mismatch: if the re-read still disagrees,
// approval stays closed instead of reading again in a loop. An equal revision with another id is never trusted.
let reconciledMismatch: string | null = null
watch(
  () => (policy.state.kind === 'success' && rules.state.kind === 'success'
    ? `${rules.state.data.policyRevision}:${policy.state.data.revision}`
    : null),
  (versions) => {
    if (!versions || rules.acting || policy.state.kind !== 'success' || rules.state.kind !== 'success') return
    const rulesRevision = rules.state.data.policyRevision
    const policyRevision = policy.state.data.revision
    if (rulesRevision === policyRevision || versions === reconciledMismatch) return
    reconciledMismatch = versions
    if (rulesRevision > policyRevision) void policy.load()
    else void rules.load()
  },
)

function sameIdentity(left: RubricIdentity, right: RubricIdentity): boolean {
  return left.packageId === right.packageId
    && left.packageRevision === right.packageRevision
    && left.path === right.path
    && left.sha256 === right.sha256
    && left.artifact.artifactId === right.artifact.artifactId
    && left.artifact.objectVersion === right.artifact.objectVersion
    && left.artifact.mediaType === right.artifact.mediaType
    && left.artifact.sizeBytes === right.artifact.sizeBytes
    && left.artifact.storeBinding === right.artifact.storeBinding
}

const rows = computed<RubricRow[]>(() => {
  const current = rules.state.kind === 'success' ? rules.state.data.rules : []
  const byPath = new Map<string, RubricRow>()
  for (const step of props.advisorySteps) {
    // Control matches the locator without its `evaluator://` scheme against the package path.
    const path = step.rubric.startsWith('evaluator://') ? step.rubric.slice('evaluator://'.length) : step.rubric
    const existing = byPath.get(path)
    if (existing) {
      existing.stepIds.push(step.id)
      continue
    }
    const file = props.problemPackage.files.find((candidate) => candidate.path === path)
    const identity: RubricIdentity | null = file
      ? { artifact: file.object, packageId: props.problemPackage.id, packageRevision: props.problemPackage.revision, path, sha256: file.sha256 }
      : null
    const rule = identity
      ? current.find((candidate) => (
        candidate.evaluationReleaseId === props.evaluationReleaseId
        && candidate.evaluationReleaseRevision === props.evaluationReleaseRevision
        && sameIdentity(candidate.rubric, identity)
      )) ?? null
      : null
    byPath.set(path, { path, stepIds: [step.id], identity, rule })
  }
  return [...byPath.values()]
})

const rowsNeedPolicy = computed(() => rows.value.some((row) => rowVisibility(row) === 'course_public' && !rowAllowsModel(row)))

const pendingDescription = computed(() => {
  const change = pending.value
  if (!change) return ''
  const effect = '之后新开始的 AI 评语调用不会再读取这个评分规则，评语可能缺少评分依据；已经开始的调用不受影响。'
  return change.action.kind === 'revoke_model_use'
    ? `撤回 ${change.row.path} 交给模型的批准。${effect}`
    : `把 ${change.row.path} 设为隐藏会同时清除已有的模型批准。${effect}需要恢复时，要重新设为公开并再次批准。`
})

/** A rubric without a stored rule is hidden on the server. */
function rowVisibility(row: RubricRow): RubricModelInputVisibility {
  return row.rule?.visibility ?? 'hidden'
}

function rowAllowsModel(row: RubricRow): boolean {
  const approval = row.rule?.approval
  const binding = syncedBinding.value
  return rowVisibility(row) === 'course_public'
    && Boolean(approval && binding && approval.model === binding.model && approval.runtimeBinding === binding.runtimeBinding)
}

function rowStatus(row: RubricRow): string {
  if (rowVisibility(row) === 'hidden') return '隐藏：不会交给模型。'
  const approval = row.rule?.approval
  if (!approval) return '课程内公开，尚未批准交给模型：不会交给模型。'
  if (rowAllowsModel(row)) return `课程内公开，已批准交给模型 ${approval.model}。`
  const binding = syncedBinding.value
  if (!binding) return `课程内公开，已批准交给模型 ${approval.model}；正在核对项目 AI 设置。`
  return `已批准的模型 ${approval.model} 与当前项目 AI 设置的模型 ${binding.model} 不同：不会交给模型。如需使用，请重新批准。`
}

async function submit(row: RubricRow, action: RubricModelInputRuleAction) {
  if (!row.identity || !canEdit.value || busy.value) return
  const result = await rules.maintain({
    action,
    evaluationReleaseId: props.evaluationReleaseId,
    evaluationReleaseRevision: props.evaluationReleaseRevision,
    rubric: row.identity,
  })
  if (result === 'denied') void permissions.refresh()
  // Another change moved the policy: the model an approval targets must be read again together with the rules.
  if (result === 'conflict') await policy.load()
}

function requestHidden(row: RubricRow) {
  const action: RubricModelInputRuleAction = { kind: 'set_visibility', visibility: 'hidden' }
  if (row.rule?.approval) pending.value = { row, action }
  else void submit(row, action)
}

function confirmPending() {
  const change = pending.value
  // Close first so a repeated confirm click cannot send a second request.
  pending.value = null
  if (change) void submit(change.row, change.action)
}

function approve(row: RubricRow) {
  const binding = syncedBinding.value
  if (!binding) return
  void submit(row, { kind: 'approve_model_use', model: binding.model, runtimeBinding: binding.runtimeBinding })
}
</script>

<style scoped>
.rubric-rules {
  display: grid;
  gap: var(--space-3);
  margin-top: var(--space-4);
  padding-top: var(--space-4);
  border-top: 1px solid var(--md-sys-color-outline-variant);
}

.rubric-rules h4 {
  margin: 0;
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-title-small);
}

.rubric-note {
  margin: 0;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-small);
}

.rubric-list {
  display: grid;
  gap: var(--space-2);
  margin: 0;
  padding: 0;
  list-style: none;
}

.rubric-row {
  display: grid;
  gap: var(--space-2);
  padding: var(--space-3);
  border-radius: var(--md-sys-shape-small);
  background: var(--md-sys-color-surface-container);
}

.rubric-heading {
  display: flex;
  flex-wrap: wrap;
  align-items: baseline;
  justify-content: space-between;
  gap: var(--space-2);
}

.rubric-path {
  overflow-wrap: anywhere;
  white-space: pre-wrap;
}

.rubric-steps,
.rubric-status {
  margin: 0;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-small);
}

.rubric-status--allowed {
  color: var(--md-sys-color-primary);
}

.rubric-status--blocked {
  color: var(--md-sys-color-error);
}

.rubric-actions {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-2);
}
</style>
