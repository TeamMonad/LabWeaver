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

    <p v-if="config.loadState === 'loading' && !config.status" class="muted" role="status">读取平台配置…</p>

    <PlatformConfigurationFailureNotice
      v-if="config.loadFailure"
      :failure="config.loadFailure"
      :dismissible="config.status !== null"
      @dismiss="config.loadFailure = null"
    />

    <template v-if="config.status">
      <div v-if="config.notice" class="notice" role="status" data-testid="pc-notice">{{ config.notice }}</div>

      <section v-if="config.conflict" class="conflict md-card" role="region" aria-labelledby="pc-conflict-heading" data-testid="pc-conflict">
        <h3 id="pc-conflict-heading">配置已被其他管理员更新</h3>
        <p>你的草稿基于 revision {{ config.baseRevision }}，服务端当前为 revision {{ config.conflict.latest.revision }}。草稿未提交且已保留。请比较后选择继续方式。</p>
        <table v-if="conflictRows.length" class="diff-table">
          <thead>
            <tr><th scope="col">字段</th><th scope="col">你的草稿</th><th scope="col">服务端最新</th><th scope="col">对方是否修改</th></tr>
          </thead>
          <tbody>
            <tr v-for="row in conflictRows" :key="row.path">
              <th scope="row"><code>{{ row.path }}</code></th>
              <td><code>{{ row.draft }}</code></td>
              <td><code>{{ row.latest }}</code></td>
              <td>{{ row.theirs ? '是' : '否' }}</td>
            </tr>
          </tbody>
        </table>
        <p v-else class="muted">你的草稿与服务端最新配置一致。</p>
        <div class="actions">
          <button type="button" class="filled-button" @click="config.rebaseDraftOnLatest">保留我的草稿，基于 revision {{ config.conflict.latest.revision }} 继续编辑</button>
          <button type="button" class="outlined-button" @click="config.discardDraftForLatest">放弃草稿，载入最新配置</button>
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
              <p v-if="yamlError" class="invalid" role="alert">{{ yamlError }}</p>
              <p class="muted">YAML 原文直接提交给服务端校验：未知字段、重复键和非法权限声明会被拒绝，错误不会回显输入。</p>
            </TabsContent>
          </TabsRoot>

          <div class="actions">
            <button type="button" class="filled-button" :disabled="!canSave" data-testid="pc-save" @click="save">
              {{ config.saving ? '保存中…' : '保存为新 desired 版本' }}
            </button>
            <button type="button" class="outlined-button" :disabled="!config.draftDirty || config.saving" @click="resetDraft">撤销修改</button>
          </div>
          <PlatformConfigurationFailureNotice
            v-if="config.actionFailure && failureSource === 'save'"
            :failure="config.actionFailure"
            @dismiss="config.actionFailure = null"
          />
          <p class="muted">保存只记录期望配置，不会部署，也不会改变 applied。</p>
        </section>

        <section class="apply md-card" aria-labelledby="pc-apply-request-heading">
          <div class="section-heading">
            <div>
              <h3 id="pc-apply-request-heading">提交应用请求</h3>
              <p v-if="config.status.desired">
                对 desired revision {{ config.status.desired.revision }} 记录应用意图。请求只登记，部署由获准执行者执行；进度以执行者报告的真实阶段为准。
              </p>
              <p v-else>尚无已保存的 desired 版本；请先保存配置。</p>
            </div>
          </div>
          <p v-if="config.draftDirty" class="muted">编辑器中有未保存修改；应用请求只针对已保存的 desired。</p>
          <button
            type="button"
            class="filled-button"
            :disabled="!config.status.desired || config.applying || config.conflict !== null"
            data-testid="pc-apply"
            @click="applyOpen = true"
          >
            提交应用请求…
          </button>
          <PlatformConfigurationFailureNotice
            v-if="config.actionFailure && failureSource === 'apply'"
            :failure="config.actionFailure"
            @dismiss="config.actionFailure = null"
          />
        </section>
      </div>

      <PlatformConfigurationApplyDialog
        v-if="config.status.desired"
        :open="applyOpen"
        :desired-revision="config.status.desired.revision"
        :impact="config.status.impact"
        :applied-state="config.status.appliedState"
        :busy="config.applying"
        @cancel="applyOpen = false"
        @confirm="confirmApply"
      />
      <PlatformConfigurationVersionDialog
        :revision="versionView.revision"
        :version="versionView.data"
        :loading="versionView.loading"
        :failure="versionView.failure"
        @close="versionView.revision = null"
      />
    </template>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue'
import { TabsContent, TabsList, TabsRoot, TabsTrigger } from 'reka-ui'
import SvgIcon from '@/components/common/SvgIcon.vue'
import PlatformConfigurationApplyDialog from '@/components/platform-configuration/PlatformConfigurationApplyDialog.vue'
import PlatformConfigurationFailureNotice from '@/components/platform-configuration/PlatformConfigurationFailureNotice.vue'
import PlatformConfigurationForm from '@/components/platform-configuration/PlatformConfigurationForm.vue'
import PlatformConfigurationStatusPanel from '@/components/platform-configuration/PlatformConfigurationStatusPanel.vue'
import PlatformConfigurationVersionDialog from '@/components/platform-configuration/PlatformConfigurationVersionDialog.vue'
import { usePlatformConfiguration, type PlatformConfigurationFailure } from '@/composables/usePlatformConfiguration'
import type { PlatformConfigurationVersionSchema } from '@/generated/contracts'
import { diffConfigurations, parseRequestYaml, requestToYaml } from '@/utils/platformConfiguration'

const config = usePlatformConfiguration()

const editorMode = ref<'form' | 'yaml'>('form')
const yamlText = ref('')
const yamlError = ref<string | null>(null)
const applyOpen = ref(false)
const failureSource = ref<'save' | 'apply'>('save')

const versionView = reactive<{
  revision: number | null
  data: PlatformConfigurationVersionSchema | null
  loading: boolean
  failure: PlatformConfigurationFailure | null
}>({ revision: null, data: null, loading: false, failure: null })

const canSave = computed(() => {
  if (config.saving || config.conflict || !config.draft) return false
  return editorMode.value === 'yaml' ? yamlText.value.trim().length > 0 : config.draftDirty || !config.status?.desired
})

const conflictRows = computed(() => {
  const conflict = config.conflict
  if (!conflict || !config.draft) return []
  const latest = conflict.latest.desired?.configuration ?? conflict.latest.defaults ?? null
  const theirs = new Set(diffConfigurations(conflict.base, latest).map((row) => row.path))
  return diffConfigurations(config.draft, latest).map((row) => ({ path: row.path, draft: row.left, latest: row.right, theirs: theirs.has(row.path) }))
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
  failureSource.value = 'save'
  const saved = await config.submitUpdate(editorMode.value, editorMode.value === 'yaml' ? yamlText.value : undefined)
  if (saved && editorMode.value === 'yaml') syncYamlFromDraft()
}

function resetDraft() {
  config.resetDraftFromServer()
  if (editorMode.value === 'yaml') syncYamlFromDraft()
}

async function confirmApply(sha256: string) {
  failureSource.value = 'apply'
  await config.requestApply(sha256)
  // Close either way so the outcome (notice or failure with recovery) is visible next to the apply action.
  applyOpen.value = false
}

async function openVersion(revision: number) {
  versionView.revision = revision
  versionView.data = null
  versionView.failure = null
  versionView.loading = true
  const outcome = await config.readVersion(revision)
  if (versionView.revision !== revision) return
  versionView.loading = false
  if ('failure' in outcome) versionView.failure = outcome.failure
  else versionView.data = outcome.data
}

async function refresh() {
  await config.load({ keepDraft: config.draftDirty })
  if (editorMode.value === 'yaml' && !config.draftDirty) syncYamlFromDraft()
}

onMounted(async () => {
  await config.load()
})
</script>

<style scoped>
.pc-page { display: grid; gap: 20px; }
.page-header, .section-heading { display: flex; align-items: flex-start; justify-content: space-between; gap: 16px; }
.page-header h2 { margin: 0; font: var(--md-sys-headline-small); color: var(--md-sys-color-on-surface); }
.section-heading h3, .conflict h3 { margin: 0; font: var(--md-sys-title-large); color: var(--md-sys-color-on-surface); }
.page-subtitle, .section-heading p, .conflict p { margin: 6px 0 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); line-height: 1.5; }
.pc-layout { display: grid; gap: 20px; grid-template-columns: minmax(0, 1fr) minmax(0, 1.4fr); align-items: start; }
.pc-layout > :first-child { grid-row: span 2; }
.editor, .apply, .conflict { display: grid; gap: 14px; padding: 20px; }
.conflict { border: 1px solid var(--md-sys-color-error); }
.notice { padding: 10px 16px; border-radius: var(--md-sys-shape-medium); background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); font: var(--md-sys-body-medium); }
.muted { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.invalid { margin: 0; color: var(--md-sys-color-error); font: var(--md-sys-body-medium); }
.actions { display: flex; flex-wrap: wrap; gap: 10px; }
.tabs { display: grid; gap: 12px; }
.tabs__list { display: flex; gap: 4px; border-bottom: 1px solid var(--md-sys-color-outline-variant); }
.tabs__trigger { min-height: 40px; padding: 0 16px; border: 0; border-bottom: 2px solid transparent; background: transparent; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-title-small); cursor: pointer; }
.tabs__trigger[data-state='active'] { border-bottom-color: var(--md-sys-color-primary); color: var(--md-sys-color-primary); }
.tabs__trigger:focus-visible { outline: 2px solid var(--md-sys-color-primary); outline-offset: -2px; }
.tabs__content { display: grid; gap: 10px; outline: none; }
.yaml-field { display: grid; gap: 6px; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.text-input { box-sizing: border-box; width: 100%; padding: 8px 11px; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface); color: var(--md-sys-color-on-surface); }
.yaml-input { font: 12px/1.5 monospace; resize: vertical; }
.diff-table { width: 100%; border-collapse: collapse; font: var(--md-sys-body-small); }
.diff-table th, .diff-table td { padding: 6px 8px; border-bottom: 1px solid var(--md-sys-color-outline-variant); text-align: left; vertical-align: top; overflow-wrap: anywhere; }
.state-chip { display: inline-flex; white-space: nowrap; padding: 4px 10px; border-radius: var(--md-sys-shape-full); background: var(--md-sys-color-surface-variant); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.filled-button { display: inline-flex; align-items: center; justify-content: center; justify-self: start; min-height: 40px; padding: 0 20px; border: 0; border-radius: var(--md-sys-shape-full); background: var(--md-sys-color-primary); color: var(--md-sys-color-on-primary); font: var(--md-sys-label-large); cursor: pointer; }
.outlined-button { min-height: 40px; padding: 0 18px; border: 1px solid var(--md-sys-color-outline); border-radius: var(--md-sys-shape-full); background: transparent; color: var(--md-sys-color-primary); font: var(--md-sys-label-large); cursor: pointer; }
.filled-button:disabled, .outlined-button:disabled, .icon-button:disabled { opacity: .5; cursor: not-allowed; }
.icon-button { display: inline-grid; place-items: center; width: 40px; height: 40px; border: 0; border-radius: 50%; background: transparent; color: var(--md-sys-color-on-surface-variant); cursor: pointer; }
@media (max-width: 1100px) { .pc-layout { grid-template-columns: 1fr; } .pc-layout > :first-child { grid-row: auto; } }
</style>
