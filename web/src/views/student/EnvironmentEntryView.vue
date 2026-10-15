<template>
  <div class="environment-entry">
    <header class="page-header">
      <h2>项目环境控制台</h2>
      <p class="page-subtitle">
        选择已发布模板创建项目环境，管理生命周期并获取 SSH/HTTP 访问授权。
      </p>
    </header>

    <DiagnosticBanner
      v-if="isContextMissing"
      code="PROJECT_CONTEXT_MISSING"
      message="项目上下文未绑定，无法加载环境模板版本。请先通过顶栏项目选择器选择有权访问的项目。"
      :retryable="false"
      severity="error"
    />


    <EnvironmentReleasePicker />

    <section
      class="console-section"
      aria-labelledby="console-heading"
    >
      <h3
        id="console-heading"
        class="section-title"
      >
        <SvgIcon
          name="desktop_windows"
          size="sm"
          aria-hidden="true"
        />
        环境控制台
      </h3>

      <details
        v-if="!isEnvironmentLoaded || environmentToolsOpen"
        class="environment-id-input-details"
        :open="environmentToolsOpen"
        @toggle="syncEnvironmentToolsOpen"
      >
        <summary>高级：输入已有环境 ID</summary>
        <div class="environment-selector">
          <label for="env-id-input">环境 ID</label>
          <input
            id="env-id-input"
            v-model="environmentIdInput"
            type="text"
            class="text-input"
            placeholder="输入环境 ID 或从创建请求获取"
          >
          <button
            type="button"
            class="filled-button"
            @click="applyEnvironmentId"
          >
            加载
          </button>
        </div>
      </details>

      <div
        v-if="!selectedEnvironmentId"
        class="placeholder-pane"
      >
        <SvgIcon
          name="info"
          size="lg"
          aria-hidden="true"
        />
        <p>{{ environmentEmptyHint }}</p>
        <RouterLink
          class="filled-button"
          :to="{ path: environmentListPath, query: projectId ? { projectId } : undefined }"
        >
          {{ environmentListActionLabel }}
        </RouterLink>
      </div>

      <template v-else>
        <div class="gcp-resource-view">
          <div class="gcp-resource-header">
            <div class="gcp-breadcrumbs">
              <RouterLink
                :to="{ path: environmentListPath, query: projectId ? { projectId } : undefined }"
                class="breadcrumb-link"
              >
                {{ breadcrumbLabel }}
              </RouterLink>
              <span class="breadcrumb-sep">/</span>
              <span
                v-if="environmentTitle"
                class="breadcrumb-current"
              >{{ environmentTitle }}</span>
            </div>
            <div class="resource-title-row">
              <div class="title-with-pill">
                <h2 v-if="environmentTitle">
                  {{ environmentTitle }}
                </h2>
                <GcpStatusPill
                  v-if="env.instance.kind === 'success'"
                  :state="env.instance.data.observedState"
                  domain="environment"
                />
              </div>
              <button
                v-if="isEnvironmentLoaded"
                type="button"
                class="text-button small"
                :aria-expanded="environmentToolsOpen"
                @click="environmentToolsOpen = !environmentToolsOpen"
              >
                <SvgIcon
                  name="tune"
                  size="sm"
                  aria-hidden="true"
                />
                <span>{{ environmentToolsLabel }}</span>
              </button>
            </div>
          </div>

          <EnvironmentLifecycleBar />

          <!-- GCP Detail Tabs -->
          <nav
            class="gcp-detail-tabs"
            aria-label="环境资源详情导航"
          >
            <button
              type="button"
              class="detail-tab"
              :class="{ 'detail-tab--active': activeTab === 'overview' }"
              @click="activeTab = 'overview'"
            >
              <SvgIcon
                name="info"
                size="sm"
                aria-hidden="true"
              />
              <span>概览与访问</span>
            </button>
            <button
              type="button"
              class="detail-tab"
              :class="{ 'detail-tab--active': activeTab === 'terminal' }"
              @click="activeTab = 'terminal'"
            >
              <SvgIcon
                name="terminal"
                size="sm"
                aria-hidden="true"
              />
              <span>Web 控制台</span>
            </button>
            <button
              type="button"
              class="detail-tab"
              :class="{ 'detail-tab--active': activeTab === 'operations' }"
              @click="activeTab = 'operations'"
            >
              <SvgIcon
                name="history"
                size="sm"
                aria-hidden="true"
              />
              <span>异步操作与诊断</span>
            </button>
            <button
              v-if="!isWorkConnection && !teacherMode"
              type="button"
              class="detail-tab"
              :class="{ 'detail-tab--active': activeTab === 'freeze' }"
              @click="activeTab = 'freeze'"
            >
              <SvgIcon
                name="lock"
                size="sm"
                aria-hidden="true"
              />
              <span>实验提交与凭据</span>
            </button>
          </nav>

          <AsyncStateView
            :state="env.instance"
            @retry="env.startPolling"
          >
            <template #success="{ data }">
              <!-- TAB 1: Overview & Endpoints -->
              <div
                v-show="activeTab === 'overview'"
                class="tab-pane"
              >
                <EnvironmentOverviewTab :data="data" />
              </div>

              <!-- TAB 2: Web Terminal -->
              <div
                v-show="activeTab === 'terminal'"
                class="tab-pane"
              >
                <EnvironmentConsoleTab :data="data" />
              </div>

              <!-- TAB 3: Operations & Timeline -->
              <div
                v-show="activeTab === 'operations'"
                class="tab-pane"
              >
                <EnvironmentOperationsTab :data="data" />
              </div>

              <!-- TAB 4: Freeze & Submission -->
              <div
                v-if="!isWorkConnection && !teacherMode"
                v-show="activeTab === 'freeze'"
                class="tab-pane"
              >
                <EnvironmentSubmissionTab :data="data" />
              </div>
            </template>
          </AsyncStateView>
        </div>
      </template>
    </section>
  </div>
</template>

<script setup lang="ts">
import { toRef } from 'vue'
import { RouterLink } from 'vue-router'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import GcpStatusPill from '@/components/common/GcpStatusPill.vue'
import SvgIcon from '@/components/common/SvgIcon.vue'
import EnvironmentConsoleTab from '@/components/environment/EnvironmentConsoleTab.vue'
import EnvironmentLifecycleBar from '@/components/environment/EnvironmentLifecycleBar.vue'
import EnvironmentOperationsTab from '@/components/environment/EnvironmentOperationsTab.vue'
import EnvironmentOverviewTab from '@/components/environment/EnvironmentOverviewTab.vue'
import EnvironmentReleasePicker from '@/components/environment/EnvironmentReleasePicker.vue'
import EnvironmentSubmissionTab from '@/components/environment/EnvironmentSubmissionTab.vue'
import { provideEnvironmentEntry, useEnvironmentEntry } from '@/composables/useEnvironmentEntry'

const props = withDefaults(defineProps<{
  /** Teacher inventory and console share the same lifecycle/access controls. */
  teacherMode?: boolean
}>(), {
  teacherMode: false,
})

const entry = useEnvironmentEntry(toRef(props, 'teacherMode'))
provideEnvironmentEntry(entry)

const {
  teacherMode,
  activeTab,
  selectedEnvironmentId,
  environmentIdInput,
  environmentToolsOpen,
  env,
  isEnvironmentLoaded,
  environmentTitle,
  projectId,
  isContextMissing,
  isWorkConnection,
  environmentListPath,
  environmentListActionLabel,
  environmentEmptyHint,
  breadcrumbLabel,
  environmentToolsLabel,
  syncEnvironmentToolsOpen,
  applyEnvironmentId,
} = entry
</script>

<style scoped>
.environment-entry {
  display: flex;
  flex-direction: column;
  gap: 24px;
}

/* GCP Resource Header & Breadcrumbs */
.gcp-resource-view {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.gcp-breadcrumbs {
  display: flex;
  align-items: center;
  gap: 8px;
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
  margin-bottom: 6px;
}

.breadcrumb-link {
  color: var(--md-sys-color-primary);
  text-decoration: none;
}

.breadcrumb-link:hover {
  text-decoration: underline;
}

.breadcrumb-sep {
  opacity: 0.5;
}

.breadcrumb-current {
  font-weight: 500;
  color: var(--md-sys-color-on-surface);
}

.resource-title-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  flex-wrap: wrap;
}

.title-with-pill {
  display: flex;
  align-items: center;
  gap: 12px;
}

.title-with-pill h2 {
  font: var(--md-sys-headline-small);
  color: var(--md-sys-color-on-surface);
  margin: 0;
  overflow-wrap: anywhere;
}

/* GCP Detail Tabs */
.gcp-detail-tabs {
  display: flex;
  align-items: center;
  gap: 4px;
  border-bottom: 1px solid var(--md-sys-color-outline-variant);
  overflow-x: auto;
  scrollbar-width: none;
}

.gcp-detail-tabs::-webkit-scrollbar {
  display: none;
}

.detail-tab {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  height: 42px;
  padding: 0 16px;
  border: none;
  background: transparent;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-label-large);
  cursor: pointer;
  white-space: nowrap;
  position: relative;
  transition: color var(--motion-duration-fast) var(--motion-ease-out);
}

.detail-tab:hover {
  color: var(--md-sys-color-on-surface);
}

.detail-tab--active {
  color: var(--md-sys-color-primary);
  font-weight: 600;
}

.detail-tab--active::after {
  content: '';
  position: absolute;
  bottom: -1px;
  left: 0;
  right: 0;
  height: 3px;
  background: var(--md-sys-color-primary);
  border-radius: 3px 3px 0 0;
}

.tab-pane {
  display: flex;
  flex-direction: column;
  gap: 20px;
  padding: 8px 0;
}

.environment-id-details,
.environment-id-input-details {
  margin: 0 16px 16px;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-label-small);
}

.environment-id-details summary,
.environment-id-input-details summary {
  cursor: pointer;
}

.page-header h2 {
  font: var(--md-sys-headline-small);
  color: var(--md-sys-color-on-surface);
  margin: 0;
}

.page-subtitle {
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface-variant);
  margin: 4px 0 0;
}

.section-title {
  display: flex;
  align-items: center;
  gap: 8px;
  font: var(--md-sys-title-medium);
  color: var(--md-sys-color-on-surface);
  margin: 0 0 12px;
}

.environment-selector {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-bottom: 16px;
}

.environment-selector label {
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface-variant);
}

.text-input {
  flex: 1;
  min-width: 0;
  height: 40px;
  padding: 0 12px;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-surface-container-low);
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-body-medium);
}

.text-input:focus-visible {
  outline: 2px solid var(--md-sys-color-primary);
  outline-offset: 2px;
}

.placeholder-pane {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 12px;
  padding: 48px 24px;
  text-align: center;
  color: var(--md-sys-color-on-surface-variant);
}

.filled-button {
  height: 40px;
  padding: 0 24px;
  border: none;
  border-radius: var(--md-sys-shape-full);
  background: var(--md-sys-color-primary);
  color: var(--md-sys-color-on-primary);
  font: var(--md-sys-label-large);
  cursor: pointer;
}

.filled-button.small {
  height: 32px;
  padding: 0 16px;
}

.filled-button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.text-button {
  height: 40px;
  padding: 0 16px;
  border: none;
  border-radius: var(--md-sys-shape-full);
  background: transparent;
  color: var(--md-sys-color-primary);
  font: var(--md-sys-label-large);
  cursor: pointer;
}

.text-button.error {
  color: var(--md-sys-color-error);
}

.text-button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.console-section {
  margin-top: 20px;
  display: flex;
  flex-direction: column;
  gap: 16px;
}
</style>
