<template>
  <div
    class="app-shell"
    :class="{ 'is-rail': drawerRail }"
    :data-theme="themeStore.effectiveTheme"
  >
    <TopAppBar
      :drawer-open="drawerOpen"
      @toggle-drawer="handleToggleDrawer"
    />

    <NavigationDrawer
      :open="drawerOpen"
      :rail="drawerRail"
      @close="drawerOpen = false"
      @toggle-rail="drawerRail = !drawerRail"
    />

    <main ref="appMain" class="app-main">
      <div
        v-if="context.state.value.kind === 'loading'"
        class="project-context-loading"
        role="status"
      >
        <span class="spinner" aria-hidden="true" />
        <span>正在确认项目…</span>
      </div>
      <section
        v-else-if="context.state.value.kind === 'list-error'"
        class="project-context-error"
        aria-labelledby="project-context-error-title"
        data-testid="project-context-list-error"
      >
        <h1 id="project-context-error-title">暂时无法确认当前项目</h1>
        <p>项目列表没有加载成功，为避免显示或操作错误项目的数据，本页暂不打开。</p>
        <DiagnosticBanner
          :diagnostic="{ ...context.state.value.diagnostic, retryable: true }"
          @retry="context.reloadProjects()"
        />
      </section>
      <ProjectContextRecovery
        v-else-if="context.state.value.kind === 'unavailable'"
        :key="context.state.value.projectId"
        :state="context.state.value"
      />
      <RouterView v-else v-slot="{ Component }">
        <!-- Re-create the page when the project or the signed-in session changes
             so no view keeps the previous project's results or the previous
             actor's forms while new requests are in flight. -->
        <component :is="Component" :key="viewKey" />
      </RouterView>
    </main>
  </div>
</template>

<script setup lang="ts">
import { computed, ref, onMounted, nextTick, watch } from 'vue'
import { useRoute } from 'vue-router'
import TopAppBar from './TopAppBar.vue'
import NavigationDrawer from './NavigationDrawer.vue'
import ProjectContextRecovery from './ProjectContextRecovery.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import { useProjectContextSync } from '@/composables/useProjectContext'
import { sessionEpoch } from '@/api/session'
import { useThemeStore } from '@/stores/theme'

const themeStore = useThemeStore()
const route = useRoute()
const appMain = ref<HTMLElement | null>(null)
const drawerOpen = ref(false)
const drawerRail = ref(false)
const context = useProjectContextSync()
const viewKey = computed(() => {
  const state = context.state.value
  return `${sessionEpoch.value}:${state.kind === 'ready' ? `project:${state.project.id}` : 'global'}`
})

function handleToggleDrawer() {
  if (typeof window !== 'undefined' && window.innerWidth >= 840) {
    drawerRail.value = !drawerRail.value
  } else {
    drawerOpen.value = !drawerOpen.value
  }
}

watch(
  () => route.fullPath,
  async () => {
    if (window.innerWidth > 760) return
    await nextTick()
    if (appMain.value) {
      appMain.value.scrollTop = 0
      appMain.value.scrollLeft = 0
    }
    if (document.scrollingElement) {
      document.scrollingElement.scrollTop = 0
      document.scrollingElement.scrollLeft = 0
    }
  },
)

onMounted(() => {
  themeStore.listenToSystemTheme()
})
</script>

<style scoped>
.app-shell {
  display: grid;
  grid-template-rows: auto var(--app-top-bar-height) 1fr;
  grid-template-columns: 1fr;
  height: 100%;
  overflow-anchor: none;
  background: var(--md-sys-color-background);
}

.top-app-bar {
  grid-row: 2;
  grid-column: 1 / -1;
}

.app-main {
  grid-row: 3;
  grid-column: 1;
  overflow-y: auto;
  padding: var(--space-6) var(--space-4) var(--space-12);
  min-width: 0;
}

.project-context-loading {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: var(--space-3);
  padding: var(--space-12) var(--space-4);
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-medium);
}

.project-context-error {
  display: grid;
  gap: var(--space-3);
  max-width: 720px;
  margin: var(--space-6) auto;
}

.project-context-error h1 {
  margin: 0;
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-headline-small);
}

.project-context-error p {
  margin: 0;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-medium);
}

.spinner {
  width: 20px;
  height: 20px;
  border: 2px solid var(--md-sys-color-outline-variant);
  border-top-color: var(--md-sys-color-primary);
  border-radius: 50%;
  animation: shell-spin 0.8s linear infinite;
}

@keyframes shell-spin {
  to { transform: rotate(360deg); }
}

@media (min-width: 840px) {
  .app-shell {
    grid-template-columns: var(--nav-drawer-expanded) 1fr;
  }

  .app-shell.is-rail {
    grid-template-columns: var(--nav-drawer-collapsed) 1fr;
  }

  .app-main {
    grid-column: 2;
    padding: var(--space-6) var(--space-8) var(--space-12);
  }
}

@media (min-width: 1200px) {
  .app-main {
    padding: var(--space-8) var(--space-12) var(--space-16);
  }
}

</style>
