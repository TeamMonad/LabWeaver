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
      <ProjectContextRecovery
        v-else-if="context.state.value.kind === 'unavailable'"
        :key="context.state.value.projectId"
        :state="context.state.value"
      />
      <RouterView v-else v-slot="{ Component }">
        <!-- Re-create project pages when the project changes so no view keeps
             the previous project's results while new requests are in flight. -->
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
import { useProjectContextSync } from '@/composables/useProjectContext'
import { useThemeStore } from '@/stores/theme'

const themeStore = useThemeStore()
const route = useRoute()
const appMain = ref<HTMLElement | null>(null)
const drawerOpen = ref(false)
const drawerRail = ref(false)
const context = useProjectContextSync()
const viewKey = computed(() => {
  const state = context.state.value
  return state.kind === 'ready' ? `project:${state.project.id}` : 'global'
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
  padding: 24px 16px 48px;
  min-width: 0;
}

.project-context-loading {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 12px;
  padding: 48px 16px;
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
    padding: 24px 32px 48px;
  }
}

@media (min-width: 1200px) {
  .app-main {
    padding: 32px 48px 64px;
  }
}

</style>
