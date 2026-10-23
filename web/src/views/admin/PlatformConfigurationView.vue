<template>
  <div class="pc-page">
    <header class="page-header">
      <div>
        <h2>平台配置</h2>
        <p class="page-subtitle">管理平台非秘密设置：生命周期、运行策略与构建策略。保存会追加一个不可变 desired 版本；应用需由获准执行者执行并验证。Secret 仍由加密 Bundle 提供，不在此页面出现。</p>
      </div>
      <button type="button" class="icon-button" aria-label="刷新平台配置" :disabled="config.loadState === 'loading'" @click="refresh">
        <SvgIcon name="refresh" size="sm" aria-hidden="true" />
      </button>
    </header>

    <AsyncStateView v-if="!config.status" :state="initialState" loading-text="读取平台配置…" @retry="config.load()" />

    <template v-else>
      <DiagnosticBanner v-if="config.loadFailure" :diagnostic="config.loadFailure" @retry="refresh" />
      <DiagnosticBanner v-if="config.notice" severity="info" :message="config.notice" data-testid="pc-notice" />

      <section v-if="config.conflict" class="conflict md-card" aria-labelledby="pc-conflict-heading" data-testid="pc-conflict">
        <h3 id="pc-conflict-heading">配置已被其他管理员更新</h3>
        <p>你的草稿基于 revision {{ config.baseRevision }}，服务端当前为 revision {{ config.conflict.latest.revision }}。草稿未提交且已保留，所选 Bundle 文件也已保留。请比较后选择继续方式。</p>
        <!-- @vue-generic {ConflictRow} -->
        <DataTable
          :columns="conflictColumns"
          :rows="conflictRows"
          aria-label="草稿与服务端最新配置的差异"
          empty-text="你的草稿与服务端最新配置一致。"
        >
          <template #path="{ row }"><code>{{ row.path }}</code></template>
          <template #draft="{ row }"><code>{{ row.draft }}</code></template>
          <template #latest="{ row }"><code>{{ row.latest }}</code></template>
          <template #theirs="{ row }">{{ row.theirs ? '是' : '否' }}</template>
        </DataTable>
        <div class="actions">
          <button type="button" class="filled-button" @click="config.rebaseDraftOnLatest">保留我的草稿，基于 revision {{ config.conflict.latest.revision }} 继续编辑</button>
          <button type="button" class="outlined-button" @click="discardDraft">放弃草稿，载入最新配置</button>
        </div>
      </section>

      <div class="pc-layout">
        <PlatformConfigurationStatusPanel
          :status="config.status"
          :history="config.applyHistory"
          :history-loading="config.historyLoading"
          :history-failure="config.historyFailure"
          @view-version="openVersion"
          @load-previous="config.loadPreviousApply"
          @refresh-apply="config.refreshApply"
        />

        <section class="editor md-card" aria-labelledby="pc-editor-heading">
          <div class="section-heading">
            <div>
              <h3 id="pc-editor-heading">编辑配置</h3>
              <p>
                基于 revision {{ config.baseRevision }}
                {{ config.status.desired ? `（desired revision ${config.status.desired.revision}）` : '（尚未配置，草稿来自服务端初始模板）' }}。
                更新会替换整个配置；表单与高级 YAML 提交同一结构和 revision。
              </p>
            </div>
            <span v-if="config.draftDirty" class="state-chip">有未保存修改</span>
          </div>

          <DiagnosticBanner
            v-if="config.draftUnsupported.length && editorMode === 'form'"
            severity="warning"
            :message="`草稿含尚无执行侧实现的设置：${config.draftUnsupported.map((field) => field.label).join('、')}。`"
            next-step="这些设置会被 Bundle 导出与部署渲染拒绝（见 #207）。恢复为可部署值后才能保存。"
            data-testid="pc-draft-unsupported"
          />
          <button
            v-if="config.draftUnsupported.length && editorMode === 'form'"
            type="button"
            class="tonal-button small"
            data-testid="pc-reset-unsupported"
            @click="resetUnsupported"
          >
            恢复为可部署值
          </button>

          <TabsRoot v-model="editorMode" class="tabs" @update:model-value="onModeChange">
            <TabsList class="tabs__list" aria-label="编辑方式">
              <TabsTrigger value="form" class="tabs__trigger">表单</TabsTrigger>
              <TabsTrigger value="yaml" class="tabs__trigger">高级 YAML</TabsTrigger>
            </TabsList>
            <TabsContent value="form" class="tabs__content">
              <PlatformConfigurationForm v-if="config.draft" v-model="config.draft" />
            </TabsContent>
            <TabsContent value="yaml" class="tabs__content">
              <label class="yaml-field">
                <span>UpdatePlatformConfigurationRequest（application/yaml）</span>
                <textarea v-model="yamlText" class="text-input yaml-input" rows="22" spellcheck="false" data-testid="pc-yaml" />
              </label>
              <DiagnosticBanner v-if="yamlError" severity="warning" :message="yamlError" />
              <p class="muted">YAML 原文直接提交给服务端校验：未知字段、重复键和非法权限声明会被拒绝，错误不会回显输入。非 0 默认容器 UID、runtime capabilities、预授权、VM guest 位置和 builder 权限尚无执行侧实现，提交前会在本地拒绝。</p>
            </TabsContent>
          </TabsRoot>

          <div class="actions">
            <button type="button" class="filled-button" :disabled="!canSave" data-testid="pc-save" @click="save">
              {{ config.saving ? '保存中…' : '保存为新 desired 版本' }}
            </button>
            <button type="button" class="outlined-button" :disabled="!config.draftDirty || config.saving" @click="resetDraft">撤销修改</button>
          </div>
          <DiagnosticBanner v-if="failureFor('save')" :diagnostic="failureFor('save')" data-testid="pc-save-failure" />
          <p class="muted">保存只记录期望配置，不会部署，也不会改变 applied。</p>
        </section>

        <section class="register md-card" aria-labelledby="pc-register-heading">
          <div class="section-heading">
            <div>
              <h3 id="pc-register-heading">登记加密 Bundle</h3>
              <p v-if="config.status.desired">
                选择与 desired revision {{ config.status.desired.revision }} 对应的实际 .age 密文文件。服务端流式计算密文 SHA-256 并绑定当前设置摘要；登记只保存可查询事实，不是批准或部署。
              </p>
              <p v-else>尚无已保存的 desired 版本；请先保存配置后再登记 Bundle。</p>
            </div>
          </div>
          <DiagnosticBanner
            v-if="config.desiredUnsupported.length"
            severity="warning"
            :message="`当前 desired 含尚无执行侧实现的设置：${config.desiredUnsupported.map((field) => field.label).join('、')}。`"
            next-step="Bundle 导出与部署渲染会拒绝此版本（见 #207），因此不能登记或应用。请在编辑器中恢复为可部署值并保存新的 desired。"
            data-testid="pc-desired-unsupported"
          />
          <p v-if="config.draftDirty" class="muted">编辑器中有未保存修改；登记与应用只针对已保存的 desired。保存新 desired 会清空现有登记。</p>
          <label class="file-field">
            <span>加密 Bundle 文件（.age）</span>
            <input
              ref="bundleInput"
              type="file"
              accept=".age,application/octet-stream"
              data-testid="pc-bundle-file"
              :disabled="!config.status.desired || config.desiredUnsupported.length > 0 || config.registering || config.conflict !== null"
              @change="onBundleSelected"
            />
          </label>
          <p v-if="config.selectedBundleFile" class="muted" data-testid="pc-bundle-file-name">
            已选择：{{ config.selectedBundleFile.name }}（{{ formatBytes(config.selectedBundleFile.size) }}）
          </p>
          <div class="actions">
            <button type="button" class="filled-button" :disabled="!config.canRegister" data-testid="pc-register" @click="config.registerBundle">
              {{ config.registering ? '登记中…' : '登记所选 Bundle' }}
            </button>
            <button type="button" class="outlined-button" :disabled="!config.selectedBundleFile || config.registering" data-testid="pc-bundle-clear" @click="clearBundle">
              清除所选文件
            </button>
          </div>
          <DiagnosticBanner v-if="failureFor('register')" :diagnostic="failureFor('register')" data-testid="pc-register-failure" />
          <p class="muted">登记参数（revision、expectedRevision、configurationSha256）取自当前服务端状态，浏览器不构造也不手填 digest。网络失败时用同一文件按原样重试，不要重新 seal。</p>
        </section>

        <section class="apply md-card" aria-labelledby="pc-apply-request-heading">
          <div class="section-heading">
            <div>
              <h3 id="pc-apply-request-heading">提交应用请求</h3>
              <p v-if="config.status.desired">
                对 desired revision {{ config.status.desired.revision }} 记录应用意图（202）。服务端绑定已登记 Bundle；部署由获准执行者执行，只有完整验证成功后 applied 才会更新。
              </p>
              <p v-else>尚无已保存的 desired 版本；请先保存配置。</p>
            </div>
          </div>
          <dl v-if="config.status.registeredBundle" class="bound-bundle" data-testid="pc-apply-registered">
            <div>
              <dt>将绑定的密文 SHA-256</dt>
              <dd><code>{{ config.status.registeredBundle.encryptedBundleSha256 }}</code></dd>
            </div>
            <div>
              <dt>设置绑定</dt>
              <dd><code>{{ config.status.registeredBundle.configurationSha256 }}</code></dd>
            </div>
          </dl>
          <p v-else-if="config.status.desired" class="muted" data-testid="pc-apply-needs-register">
            当前 desired 尚无 Bundle 登记；请先完成登记后再提交应用请求。
          </p>
          <DiagnosticBanner
            v-if="config.status.appliedState === 'unverified'"
            severity="warning"
            message="当前外部结果待核对。"
            next-step="新的应用尝试不会清除待核对状态；请先核对外部真实状态再继续获准执行。"
          />
          <div class="actions">
            <button type="button" class="filled-button" :disabled="!config.canApply" data-testid="pc-apply" @click="applyOpen = true">
              {{ config.applying ? '提交中…' : '提交应用请求…' }}
            </button>
          </div>
          <DiagnosticBanner v-if="failureFor('apply')" :diagnostic="failureFor('apply')" data-testid="pc-apply-failure" />
        </section>
      </div>

      <ConfirmDialog
        :open="applyOpen"
        :title="`记录 desired revision ${config.status.desired?.revision ?? ''} 的应用请求`"
        :description="applyDescription"
        confirm-text="记录应用请求"
        severity="warning"
        @cancel="applyOpen = false"
        @confirm="confirmApply"
      />
      <EvidenceSideSheet :open="versionView.revision !== null" :title="`配置 revision ${versionView.revision ?? ''}`" @close="versionView.revision = null">
        <AsyncStateView :state="versionState" loading-text="读取版本…" @retry="versionView.revision !== null && openVersion(versionView.revision)">
          <template #success="{ data }">
            <p class="muted">{{ formatTimestamp(data.createdAt) }} · {{ data.createdBy }}</p>
            <p class="muted">设置摘要 <code>{{ data.configurationSha256 }}</code></p>
            <pre class="version-yaml" data-testid="pc-version-yaml">{{ stringify(data.configuration, { lineWidth: 0 }) }}</pre>
          </template>
        </AsyncStateView>
      </EvidenceSideSheet>
    </template>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue'
import { TabsContent, TabsList, TabsRoot, TabsTrigger } from 'reka-ui'
import { stringify } from 'yaml'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import ConfirmDialog from '@/components/common/ConfirmDialog.vue'
import DataTable, { type DataTableColumn } from '@/components/common/DataTable.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import EvidenceSideSheet from '@/components/common/EvidenceSideSheet.vue'
import SvgIcon from '@/components/common/SvgIcon.vue'
import PlatformConfigurationForm from '@/components/platform-configuration/PlatformConfigurationForm.vue'
import PlatformConfigurationStatusPanel from '@/components/platform-configuration/PlatformConfigurationStatusPanel.vue'
import { usePlatformConfiguration, type PlatformConfigurationAction } from '@/composables/usePlatformConfiguration'
import type { PlatformConfigurationVersionSchema } from '@/generated/contracts'
import type { AsyncState, DiagnosticViewModel } from '@/types/async'
import { formatBytes, formatTimestamp } from '@/utils/format'
import { diffConfigurations, parseRequestYaml, requestToYaml, withoutUnsupportedPolicy } from '@/utils/platformConfiguration'

interface ConflictRow { key: string; path: string; draft: string; latest: string; theirs: boolean }

const config = usePlatformConfiguration()

const editorMode = ref<'form' | 'yaml'>('form')
const yamlText = ref('')
const yamlError = ref<string | null>(null)
const applyOpen = ref(false)
const bundleInput = ref<HTMLInputElement | null>(null)

const versionView = reactive<{
  revision: number | null
  data: PlatformConfigurationVersionSchema | null
  failure: DiagnosticViewModel | null
}>({ revision: null, data: null, failure: null })

const initialState = computed<AsyncState<never>>(() =>
  config.loadState === 'error' && config.loadFailure
    ? { kind: 'error', diagnostic: config.loadFailure }
    : { kind: 'loading' },
)

const versionState = computed<AsyncState<PlatformConfigurationVersionSchema>>(() => {
  if (versionView.failure) return { kind: 'error', diagnostic: versionView.failure }
  return versionView.data ? { kind: 'success', data: versionView.data } : { kind: 'loading' }
})

function failureFor(action: PlatformConfigurationAction): DiagnosticViewModel | null {
  return config.actionFailure?.action === action ? config.actionFailure.diagnostic : null
}

const canSave = computed(() => {
  if (config.saving || config.conflict || !config.draft) return false
  if (editorMode.value === 'yaml') return yamlText.value.trim().length > 0
  return config.draftUnsupported.length === 0 && (config.draftDirty || !config.status?.desired)
})

const conflictColumns: DataTableColumn<ConflictRow>[] = [
  { key: 'path', title: '字段' },
  { key: 'draft', title: '你的草稿' },
  { key: 'latest', title: '服务端最新' },
  { key: 'theirs', title: '对方是否修改' },
]

const conflictRows = computed<ConflictRow[]>(() => {
  const conflict = config.conflict
  if (!conflict || !config.draft) return []
  const latest = conflict.latest.desired?.configuration ?? conflict.latest.defaults ?? null
  const theirs = new Set(diffConfigurations(conflict.base, latest).map((row) => row.path))
  return diffConfigurations(config.draft, latest).map((row) => ({ key: row.path, path: row.path, draft: row.left, latest: row.right, theirs: theirs.has(row.path) }))
})

const applyDescription = computed(() => {
  const registered = config.status?.registeredBundle
  const digest = registered ? `已登记密文 ${registered.encryptedBundleSha256.slice(0, 12)}…` : '已登记 Bundle'
  return `只记录一次应用请求并绑定${digest}，浏览器不会执行部署。获准执行者使用既有部署入口执行并报告校验、应用、验证阶段；只有完整验证成功后 applied 才会更新。`
})

function syncYamlFromDraft() {
  if (!config.draft) return
  yamlText.value = requestToYaml({ expectedRevision: config.baseRevision, configuration: config.draft })
  yamlError.value = null
}

function onModeChange(mode: string | number) {
  if (mode === 'yaml') {
    syncYamlFromDraft()
    return
  }
  // Leaving YAML: carry valid edits back into the form draft; keep YAML open on a parse error.
  const parsed = parseRequestYaml(yamlText.value)
  if (!parsed.ok) {
    yamlError.value = `${parsed.message} 已停留在 YAML，修正后再切换。`
    editorMode.value = 'yaml'
    return
  }
  config.draft = parsed.request.configuration
  yamlError.value = null
}

async function save() {
  const saved = await config.submitUpdate(editorMode.value, editorMode.value === 'yaml' ? yamlText.value : undefined)
  if (saved && editorMode.value === 'yaml') syncYamlFromDraft()
}

function resetDraft() {
  config.resetDraftFromServer()
  if (editorMode.value === 'yaml') syncYamlFromDraft()
}

function resetUnsupported() {
  if (config.draft) config.draft = withoutUnsupportedPolicy(config.draft)
}

function discardDraft() {
  config.discardDraftForLatest()
  if (editorMode.value === 'yaml') syncYamlFromDraft()
}

function onBundleSelected(event: Event) {
  config.selectBundleFile((event.target as HTMLInputElement).files?.[0] ?? null)
}

function clearBundle() {
  config.selectBundleFile(null)
  if (bundleInput.value) bundleInput.value.value = ''
}

async function confirmApply() {
  applyOpen.value = false
  await config.requestApply()
}

async function openVersion(revision: number) {
  versionView.revision = revision
  versionView.data = null
  versionView.failure = null
  const outcome = await config.readVersion(revision)
  if (versionView.revision !== revision) return
  if ('failure' in outcome) versionView.failure = outcome.failure
  else versionView.data = outcome.data
}

async function refresh() {
  await config.load({ keepDraft: config.draftDirty })
  if (editorMode.value === 'yaml' && !config.draftDirty) syncYamlFromDraft()
}

onMounted(() => config.load())
</script>

<style scoped>
.pc-page { display: grid; gap: var(--space-5); }
.page-header, .section-heading { display: flex; align-items: flex-start; justify-content: space-between; gap: var(--space-4); }
.page-header h2 { margin: 0; font: var(--md-sys-headline-small); color: var(--md-sys-color-on-surface); }
.section-heading h3, .conflict h3 { margin: 0; font: var(--md-sys-title-large); color: var(--md-sys-color-on-surface); }
.page-subtitle, .section-heading p, .conflict p { margin: var(--space-2) 0 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); line-height: 1.5; }
.pc-layout { display: grid; gap: var(--space-5); grid-template-columns: minmax(0, 1fr) minmax(0, 1.4fr); align-items: start; }
.pc-layout > :first-child { grid-row: span 3; }
.editor, .register, .apply, .conflict { display: grid; gap: var(--space-4); padding: var(--space-5); min-width: 0; }
.editor > button { justify-self: start; }
.conflict { border: 1px solid var(--md-sys-color-error); }
.file-field { display: grid; gap: var(--space-2); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.file-field input[type="file"] { font: var(--md-sys-body-medium); color: var(--md-sys-color-on-surface); }
.bound-bundle { display: grid; gap: var(--space-2); margin: 0; padding: var(--space-3); border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low); }
.bound-bundle dt { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.bound-bundle dd { margin: var(--space-1) 0 0; overflow-wrap: anywhere; font: var(--md-sys-body-small); font-family: var(--md-sys-font-family-mono); }
.muted { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); overflow-wrap: anywhere; }
.actions { display: flex; flex-wrap: wrap; gap: var(--space-3); }
.tabs { display: grid; gap: var(--space-3); }
.tabs__list { display: flex; gap: var(--space-1); border-bottom: 1px solid var(--md-sys-color-outline-variant); }
.tabs__trigger { min-height: 44px; padding: 0 var(--space-4); border: 0; border-bottom: 3px solid transparent; background: transparent; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-title-small); cursor: pointer; }
.tabs__trigger[data-state='active'] { border-bottom-color: var(--md-sys-color-primary); color: var(--md-sys-color-primary); }
.tabs__trigger:focus-visible { outline: 2px solid var(--md-sys-color-primary); outline-offset: -2px; }
.tabs__content { display: grid; gap: var(--space-3); outline: none; }
.yaml-field { display: grid; gap: var(--space-2); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.yaml-input { font: var(--md-sys-body-small); font-family: var(--md-sys-font-family-mono); }
.version-yaml { margin: var(--space-3) 0 0; padding: var(--space-3); overflow: auto; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container); font: var(--md-sys-body-small); font-family: var(--md-sys-font-family-mono); white-space: pre; }
.state-chip { display: inline-flex; white-space: nowrap; padding: var(--space-1) var(--space-3); border-radius: var(--md-sys-shape-full); background: var(--md-sys-color-surface-variant); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
@media (max-width: 1100px) { .pc-layout { grid-template-columns: 1fr; } .pc-layout > :first-child { grid-row: auto; } }
</style>
