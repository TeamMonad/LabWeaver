<template>
  <section class="manual-publisher" aria-labelledby="manual-publisher-heading" data-testid="manual-publisher">
    <h3 id="manual-publisher-heading">发布手工 Work 模板</h3>
    <p class="hint">
      从管理员登记的固定平台镜像直接发布 Work 模板，不需要模型或 Agent。发布需要项目编辑职责，并且是项目所有者、平台管理员或有项目管理（管理成员）职责；平台会再次检查。
    </p>

    <p v-if="publishBlockReason" class="hint hint--blocked" role="note" data-testid="manual-permission-hint">{{ publishBlockReason }}</p>
    <AsyncStateView v-else :state="publication.images" empty-text="当前没有可用的平台镜像。请联系平台管理员导入并固定镜像。" @retry="publication.loadImages">
      <template #success="{ data: images }">
        <p v-if="restoredPending" class="hint hint--blocked" role="status" data-testid="manual-pending">
          上次发布的结果尚未确认，已恢复同一发布内容。请重新确认后提交；内容不变时平台沿用同一请求，不会重复发布。
        </p>
        <AsyncStateView :state="publication.options" empty-text="当前没有可用的运行位置。请联系平台管理员检查运行配置。" @retry="publication.loadOptions">
        <template #success="{ data: options }">
        <form class="manual-form" @submit.prevent="submit">
          <label v-if="manualReleases.length > 0" class="field">
            <span>以已有手工模板为起点（可选）</span>
            <AppSelect v-model="baseReleaseKey" data-testid="manual-base-release">
              <option value="">从空白开始</option>
              <option v-for="release in manualReleases" :key="`${release.id}:${release.version}`" :value="`${release.id}:${release.version}`">
                {{ workTemplateTitle(release) }} · {{ release.templateFamilyId }}
              </option>
            </AppSelect>
            <small>复制该版本已审核的配置，用于发布同一模板系列的新版本。</small>
          </label>

          <fieldset class="field-group">
            <legend>平台镜像</legend>
            <RadioGroupRoot v-model="draft.imageId" class="image-list" aria-label="平台镜像">
              <RadioGroupItem
                v-for="image in images"
                :key="image.catalogId"
                :value="image.catalogId"
                class="image-option"
                :data-image-id="image.catalogId"
              >
                <span class="image-option__radio" aria-hidden="true"><RadioGroupIndicator class="image-option__dot" /></span>
                <span class="image-option__body">
                  <strong>{{ image.binding }} · {{ image.kind === 'virtual_machine' ? '虚拟机' : '容器' }}</strong>
                  <span>{{ image.sourceReference }}</span>
                  <span>{{ shortDigest(image.resolvedDigest) }}{{ image.kind === 'virtual_machine' && image.capacityBytes ? ` · 磁盘 ${Math.round(image.capacityBytes / GIB)} GiB` : '' }}</span>
                </span>
              </RadioGroupItem>
            </RadioGroupRoot>
          </fieldset>

          <fieldset class="field-group">
            <legend>名称与用途</legend>
            <label class="field"><span>模板名称</span><input v-model="draft.name" class="text-input" maxlength="120" required /></label>
            <label class="field">
              <span>模板系列标识</span>
              <input v-model="draft.templateFamilyId" class="text-input" maxlength="128" required spellcheck="false" />
              <small>同一系列的较新版本可用于升级容器 Work。只能使用字母、数字、点、下划线和连字符。</small>
            </label>
            <label class="field"><span>说明（用途、预装软件）</span><textarea v-model="draft.description" class="text-input" rows="3" maxlength="4000" /></label>
          </fieldset>

          <fieldset class="field-group">
            <legend>默认资源</legend>
            <div class="row">
              <label class="field"><span>CPU（m）</span><input v-model.number="draft.cpuMillicores" class="text-input" type="number" min="1" required /></label>
              <label class="field"><span>内存（GiB）</span><input v-model.number="draft.memoryGiB" class="text-input" type="number" min="1" required /></label>
              <label class="field"><span>存储（GiB）</span><input v-model.number="draft.storageGiB" class="text-input" type="number" min="1" required /></label>
            </div>
          </fieldset>

          <fieldset v-if="selectedImage?.kind !== 'virtual_machine'" class="field-group">
            <legend>入口</legend>
            <div class="row">
              <label class="field">
                <span>应用协议</span>
                <AppSelect v-model="draft.entryProtocol"><option value="http">HTTP</option><option value="https">HTTPS</option></AppSelect>
              </label>
              <label class="field"><span>应用端口</span><input v-model.number="draft.servicePort" class="text-input" type="number" min="1" max="65535" required /></label>
              <label class="field">
                <span>网络</span>
                <AppSelect v-model="draft.network"><option value="deny_all">不能访问外部网络</option><option value="allow_all">可以访问外部网络</option></AppSelect>
              </label>
            </div>
            <label class="checkbox"><input v-model="draft.terminal" type="checkbox" /> 提供浏览器终端（在 /workspace 中运行）</label>
            <label v-if="draft.terminal" class="field"><span>终端程序</span><input v-model="draft.terminalExecutable" class="text-input" spellcheck="false" /></label>
          </fieldset>
          <p v-else class="hint">虚拟机模板通过 SSH（端口 22）和桌面连接，不能访问外部网络。</p>

          <fieldset class="field-group">
            <legend>运行与存储</legend>
            <div class="row">
              <label class="field">
                <span>运行位置</span>
                <AppSelect v-model="draft.providerBinding" data-testid="manual-provider" required>
                  <option value="">请选择运行位置</option>
                  <option v-for="provider in providers" :key="provider.binding" :value="provider.binding">{{ provider.label }} · {{ provider.binding }}</option>
                </AppSelect>
                <small>{{ selectedProvider?.description }}</small>
              </label>
              <label class="field"><span>安全配置标签</span><input v-model="draft.securityProfileBinding" class="text-input" spellcheck="false" required /><small>此标签不选择或改变执行侧安全策略。实际权限由已审核的安全配置与平台策略决定。</small></label>
              <label v-if="selectedImage?.kind === 'virtual_machine'" class="field">
                <span>磁盘存储</span>
                <AppSelect v-model="draft.storageClassBinding" data-testid="manual-storage" required>
                  <option value="">请选择磁盘存储</option>
                  <option v-for="storage in selectedProvider?.storageClasses ?? []" :key="storage.binding" :value="storage.binding">{{ storage.label }} · {{ storage.binding }}</option>
                </AppSelect>
                <small>{{ selectedStorage?.description }}</small>
              </label>
            </div>
            <p v-if="!providers.length" class="hint hint--blocked" role="status">当前没有支持此镜像类型的运行位置，请联系平台管理员。</p>
          </fieldset>

          <fieldset class="field-group">
            <legend>审核</legend>
            <label class="field">
              <span>材料保留策略</span>
              <AppSelect v-model="draft.retentionPolicyId" data-testid="manual-retention" required>
                <option value="">请选择材料保留策略</option>
                <option :value="options.retention.policyId">{{ options.retention.label }} · {{ options.retention.policyId }}</option>
              </AppSelect>
              <small>{{ options.retention.description }}</small>
            </label>
            <label class="field">
              <span>材料保留至</span>
              <input v-model="draft.retainUntil" class="text-input" type="date" required />
              <small>由这个版本创建的 Work 的材料保留期限。</small>
            </label>
            <label class="field"><span>审核说明</span><textarea v-model="draft.reason" class="text-input" rows="2" maxlength="500" required /></label>
            <label class="checkbox"><input v-model="draft.confirmed" type="checkbox" data-testid="manual-confirm" /> 我已核对镜像、资源、入口和安全配置，确认发布这个固定版本</label>
          </fieldset>

          <p v-if="problem" class="hint hint--blocked" role="note" data-testid="manual-problem">{{ problem }}</p>
          <div class="actions">
            <button type="submit" class="filled-button" :disabled="Boolean(problem) || publication.publishing" data-testid="manual-publish">
              {{ publication.publishing ? '发布中…' : '发布模板' }}
            </button>
            <span class="hint">发布后版本不可修改；新的 Work 可以选择它，已有 Work 不会被替换。</span>
          </div>
        </form>
        </template>
        </AsyncStateView>
      </template>
    </AsyncStateView>

    <p v-if="publication.outcome?.kind === 'accepted'" class="hint" role="status" data-testid="manual-accepted">
      发布已受理。版本出现在上方模板列表后即可选择。
    </p>
    <DiagnosticBanner
      v-else-if="publication.outcome?.kind === 'failed'"
      :diagnostic="publication.outcome.diagnostic"
      @retry="submit"
    />
    <p
      v-if="publication.outcome?.kind === 'failed' && publication.outcome.unconfirmed"
      class="hint hint--blocked"
      role="status"
      data-testid="manual-unconfirmed"
    >
      发布结果待确认：平台可能已经受理这次发布。点击“重试”或再次提交相同内容会沿用同一请求，不会重复发布。
    </p>
  </section>
</template>

<script setup lang="ts">
import { computed, reactive, ref, toRef, watch } from 'vue'
import { RadioGroupIndicator, RadioGroupItem, RadioGroupRoot } from 'reka-ui'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import AppSelect from '@/components/form/AppSelect.vue'
import type { EnvironmentTemplateReleaseViewSchema } from '@/generated/contracts'
import { useAuth } from '@/composables/useAuth'
import {
  draftFromManualRelease,
  emptyManualDraft,
  manualDraftProblem,
  useManualWorkTemplatePublication,
  type TemplateImage,
} from '@/composables/useManualWorkTemplatePublication'
import { useProjectAvailableActions } from '@/composables/useProjectAvailableActions'
import { actorIdFromProfile, hasAnyRole, rolesFromProfile } from '@/utils/navigation'
import { shortDigest, workTemplateTitle } from '@/utils/workTemplate'

const props = defineProps<{
  projectId: string
  courseId?: string | null
  /** The project's owner, from the project read model. */
  projectOwnerId?: string | null
  /** Releases already visible in this project; manual ones can seed a new version. */
  releases: EnvironmentTemplateReleaseViewSchema[]
}>()
const emit = defineEmits<{ published: [] }>()

const GIB = 1024 ** 3
const auth = useAuth()
const actorId = computed(() => actorIdFromProfile(auth.user.value?.profile) ?? null)
const publication = useManualWorkTemplatePublication(toRef(props, 'projectId'), computed(() => props.courseId ?? null), actorId)
const permissions = useProjectAvailableActions(toRef(props, 'projectId'))
const draft = reactive(emptyManualDraft())
const baseReleaseKey = ref('')
const restoredPending = ref(false)

/**
 * Control checks publication as the `edit` object action, and the publication
 * itself additionally requires the project owner, a platform administrator or
 * the member-management responsibility. The page only mirrors those facts;
 * the server decides again on every request.
 */
const publishBlockReason = computed(() => {
  const denied = permissions.denialReason('edit', '发布 Work 模板')
  if (denied) return denied
  if (!actorId.value) return '正在确认登录身份，确认后才能发布 Work 模板。'
  const governs = props.projectOwnerId === actorId.value
    || hasAnyRole(rolesFromProfile(auth.user.value?.profile), ['admin'])
    || permissions.allows('manage_members')
  return governs ? null : '发布 Work 模板需要项目所有者、平台管理员或项目管理（管理成员）职责。如需发布，请联系项目负责人。'
})

const manualReleases = computed(() => props.releases.filter((release) => release.manualApproval && !release.withdrawal))
const selectedImage = computed<TemplateImage | null>(() => (
  publication.images.kind === 'success' ? publication.images.data.find((image) => image.catalogId === draft.imageId) ?? null : null
))
const providers = computed(() => publication.options.kind === 'success'
  ? publication.options.data.runtime.providers.filter((provider) => provider.runtimeKind === selectedImage.value?.kind) : [])
const selectedProvider = computed(() => providers.value.find((provider) => provider.binding === draft.providerBinding))
const selectedStorage = computed(() => selectedProvider.value?.storageClasses.find((storage) => storage.binding === draft.storageClassBinding))
const problem = computed(() => {
  if (!selectedImage.value) return manualDraftProblem(draft, null)
  if (!selectedProvider.value) return '请选择当前可用的运行位置。'
  if (selectedImage.value?.kind === 'virtual_machine' && !selectedStorage.value) return '请选择当前运行位置支持的磁盘存储。'
  if (publication.options.kind !== 'success' || draft.retentionPolicyId !== publication.options.data.retention.policyId) return '请选择当前平台材料保留策略。'
  const deadline = Date.parse(`${draft.retainUntil}T00:00:00.000Z`)
  if (deadline - Date.now() > publication.options.data.retention.maxRetentionSeconds * 1000) return '材料保留日期超过平台允许的最长时间，请选择更早日期。'
  return manualDraftProblem(draft, selectedImage.value)
})

watch(() => draft.retentionPolicyId, (id) => {
  if (publication.options.kind === 'success' && id === publication.options.data.retention.policyId) {
    draft.retentionPolicyRevision = publication.options.data.retention.policyRevision
  }
})

watch(() => props.projectId, () => {
  Object.assign(draft, emptyManualDraft())
  baseReleaseKey.value = ''
})

// A publication whose outcome is unknown comes back with its exact content; the reviewer confirms again.
watch(() => publication.pendingDraft, (pending) => {
  restoredPending.value = Boolean(pending)
  if (pending) Object.assign(draft, { ...pending, confirmed: false })
}, { immediate: true })

watch(baseReleaseKey, (key) => {
  const release = manualReleases.value.find((item) => `${item.id}:${item.version}` === key)
  const images = publication.images.kind === 'success' ? publication.images.data : []
  Object.assign(draft, (release && draftFromManualRelease(release, images)) ?? emptyManualDraft())
})

async function submit() {
  if (publishBlockReason.value || problem.value) return
  if (await publication.publish({ ...draft })) {
    Object.assign(draft, emptyManualDraft())
    baseReleaseKey.value = ''
    restoredPending.value = false
    emit('published')
    return
  }
  // A refused publication may mean the actor's project responsibility changed.
  if (publication.outcome?.kind === 'failed' && publication.outcome.diagnostic.status === 403) void permissions.refresh()
}
</script>

<style scoped>
.manual-publisher { display: grid; gap: var(--space-3); }
.manual-publisher h3 { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-small); }
.hint { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.hint--blocked { color: var(--md-sys-color-on-surface); }
.manual-form { display: grid; gap: var(--space-4); }
.field-group { display: grid; gap: var(--space-2); min-width: 0; margin: 0; padding: 0; border: 0; }
.field-group legend { margin-bottom: var(--space-1); color: var(--md-sys-color-on-surface); font: var(--md-sys-label-large); }
.field { display: grid; gap: var(--space-1); min-width: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.field small { color: var(--md-sys-color-on-surface-variant); }
.row { display: grid; grid-template-columns: repeat(auto-fit, minmax(180px, 1fr)); gap: var(--space-3); }
.checkbox { display: flex; align-items: flex-start; gap: var(--space-2); color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); }
.image-list { display: grid; gap: var(--space-2); }
.image-option { display: flex; gap: var(--space-3); align-items: flex-start; padding: var(--space-3); border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-medium); background: var(--md-sys-color-surface); color: inherit; font: inherit; text-align: left; cursor: pointer; }
.image-option[data-state='checked'] { border-color: var(--md-sys-color-primary); background: var(--md-sys-color-primary-container); }
.image-option:focus-visible { outline: 2px solid var(--md-sys-color-primary); outline-offset: 2px; }
.image-option__radio { display: inline-grid; flex-shrink: 0; place-items: center; width: 20px; height: 20px; border: 2px solid var(--md-sys-color-outline); border-radius: 50%; }
.image-option__dot { width: 10px; height: 10px; border-radius: 50%; background: var(--md-sys-color-primary); }
.image-option__body { display: grid; gap: 2px; min-width: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); overflow-wrap: anywhere; }
.image-option__body strong { color: var(--md-sys-color-on-surface); font: var(--md-sys-title-small); }
.actions { display: flex; flex-wrap: wrap; align-items: center; gap: var(--space-2) var(--space-4); }
</style>
