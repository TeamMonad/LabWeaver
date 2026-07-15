<template>
  <div
    class="app-shell"
    :class="{ 'is-rail': drawerRail }"
    :data-theme="themeStore.effectiveTheme"
  >
    <TopAppBar
      :drawer-open="drawerOpen"
      @toggle-drawer="drawerOpen = !drawerOpen"
    />

    <NavigationDrawer
      :open="drawerOpen"
      :rail="drawerRail"
      @close="drawerOpen = false"
      @toggle-rail="drawerRail = !drawerRail"
    />

    <main class="app-main">
      <RouterView />
    </main>
  </div>
</template>

<script setup lang="ts">
import { ref, onMounted } from 'vue'
import TopAppBar from './TopAppBar.vue'
import NavigationDrawer from './NavigationDrawer.vue'
import { useThemeStore } from '@/stores/theme'
import { useAuth } from '@/composables/useAuth'

const themeStore = useThemeStore()
const auth = useAuth()
const drawerOpen = ref(false)
const drawerRail = ref(false)

onMounted(() => {
  themeStore.listenToSystemTheme()
  void auth.loadUser()
})
</script>

<style scoped>
.app-shell {
  display: grid;
  grid-template-rows: var(--app-top-bar-height) 1fr;
  grid-template-columns: 1fr;
  height: 100%;
  background: var(--md-sys-color-background);
}

.top-app-bar {
  grid-row: 1;
  grid-column: 1 / -1;
}

.app-main {
  grid-row: 2;
  grid-column: 1;
  overflow-y: auto;
  padding: 24px 16px 48px;
  min-width: 0;
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
