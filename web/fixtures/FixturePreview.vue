<template>
  <div class="fixture-preview">
    <AppShell />

    <div
      class="fixture-ribbon"
      role="status"
    >
      <strong>FIXTURE PREVIEW</strong>
      <span>{{ activeScene.label }}</span>
      <button
        type="button"
        class="fixture-ribbon__toggle"
        @click="panelOpen = !panelOpen"
      >
        {{ panelOpen ? '收起场景' : '打开场景' }}
      </button>
    </div>

    <aside
      v-if="panelOpen"
      class="fixture-panel md-card"
      aria-label="Fixture 场景选择器"
    >
      <header class="fixture-panel__header">
        <div>
          <p class="fixture-kicker">
            本地视觉预览
          </p>
          <h1>场景画廊</h1>
        </div>
        <span class="fixture-count">{{ fixtureScenes.length }} 个场景</span>
      </header>
      <p class="fixture-panel__hint">
        数据来自独立 fixture 边界；不会调用真实登录或写入接口。
      </p>

      <p
        v-if="auth.error.value"
        class="fixture-notice"
        role="alert"
      >
        {{ auth.error.value.message }}
      </p>

      <nav
        class="fixture-scene-list"
        aria-label="Fixture 场景"
      >
        <section
          v-for="group in sceneGroups"
          :key="group.label"
          class="fixture-scene-group"
        >
          <h2>{{ group.label }}</h2>
          <button
            v-for="scene in group.scenes"
            :key="scene.id"
            type="button"
            class="fixture-scene"
            :class="{ 'fixture-scene--active': scene.id === activeScene.id }"
            :aria-current="scene.id === activeScene.id ? 'page' : undefined"
            @click="selectScene(scene.id)"
          >
            <span class="fixture-scene__label">{{ scene.label }}</span>
            <span class="fixture-scene__description">{{ scene.description }}</span>
          </button>
        </section>
      </nav>
    </aside>
  </div>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import AppShell from '@/components/layout/AppShell.vue'
import { useAuth } from '@/composables/useAuth'
import { activeScene, fixtureScenes, sceneById, setFixtureScene, type FixtureScene } from './scenes'

const router = useRouter()
const route = useRoute()
const auth = useAuth()
const panelOpen = ref(false)

const sceneGroups = computed(() => {
  const groups = new Map<string, FixtureScene[]>()
  for (const scene of fixtureScenes) {
    const items = groups.get(scene.group) ?? []
    items.push(scene)
    groups.set(scene.group, items)
  }
  return Array.from(groups, ([label, scenes]) => ({ label, scenes }))
})

watch(
  () => route.query.scene,
  (id) => {
    if (typeof id === 'string') setFixtureScene(id)
  },
  { immediate: true },
)

watch(
  () => activeScene.value.label,
  (label) => {
    document.title = `${label} · LabWeaver Fixture`
  },
  { immediate: true },
)

async function selectScene(id: string) {
  const scene = sceneById(id)
  setFixtureScene(scene.id)
  await router.push({
    path: scene.path,
    query: {
      scene: scene.id,
      ...(scene.projectId ? { projectId: scene.projectId } : {}),
      ...(scene.environmentId ? { environmentId: scene.environmentId } : {}),
    },
  })
}
</script>

<style>
.fixture-preview {
  min-height: 100%;
  background: var(--md-sys-color-background);
}

.fixture-ribbon {
  position: fixed;
  left: 16px;
  bottom: 16px;
  z-index: 1000;
  display: flex;
  align-items: center;
  gap: 8px;
  max-width: min(420px, calc(100vw - 32px));
  padding: 6px 10px;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-full);
  background: var(--md-sys-color-tertiary-container);
  color: var(--md-sys-color-on-tertiary-container);
  font: var(--md-sys-label-small);
  box-shadow: var(--md-sys-elevation-1);
}

.fixture-ribbon strong {
  letter-spacing: .05em;
}

.fixture-ribbon__toggle {
  min-height: 24px;
  padding: 0 8px;
  border: 1px solid currentColor;
  border-radius: var(--md-sys-shape-full);
  background: transparent;
  color: inherit;
  font: var(--md-sys-label-small);
  cursor: pointer;
}

.fixture-panel {
  position: fixed;
  right: 16px;
  bottom: 16px;
  z-index: 1050;
  display: grid;
  width: min(360px, calc(100vw - 32px));
  max-height: min(660px, calc(100vh - 120px));
  gap: 12px;
  padding: 16px;
  overflow: hidden;
}

.fixture-panel__header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 12px;
}

.fixture-kicker {
  color: var(--md-sys-color-primary);
  font: var(--md-sys-label-small);
  letter-spacing: .05em;
  text-transform: uppercase;
}

.fixture-panel h1 {
  margin: 2px 0 0;
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-title-large);
}

.fixture-count {
  flex: 0 0 auto;
  padding: 4px 8px;
  border-radius: var(--md-sys-shape-full);
  background: var(--md-sys-color-surface-container-high);
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-label-small);
}

.fixture-panel__hint {
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-small);
  line-height: 1.45;
}

.fixture-notice {
  padding: 8px 10px;
  border-radius: var(--md-sys-shape-small);
  background: var(--md-sys-color-error-container);
  color: var(--md-sys-color-on-error-container);
  font: var(--md-sys-body-small);
}

.fixture-scene-list {
  display: grid;
  gap: 14px;
  min-height: 0;
  overflow-y: auto;
  padding-right: 2px;
}

.fixture-scene-group {
  display: grid;
  gap: 6px;
}

.fixture-scene-group h2 {
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-label-medium);
}

.fixture-scene {
  display: grid;
  gap: 2px;
  width: 100%;
  padding: 9px 10px;
  border: 1px solid transparent;
  border-radius: var(--md-sys-shape-small);
  background: transparent;
  color: var(--md-sys-color-on-surface);
  text-align: left;
  cursor: pointer;
}

.fixture-scene:hover,
.fixture-scene--active {
  border-color: var(--md-sys-color-primary);
  background: var(--md-sys-color-primary-container);
  color: var(--md-sys-color-on-primary-container);
}

.fixture-scene__label {
  font: var(--md-sys-label-large);
}

.fixture-scene__description {
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-small);
  line-height: 1.35;
}

.fixture-scene--active .fixture-scene__description {
  color: var(--md-sys-color-on-primary-container);
}

@media (max-width: 900px) {
  .fixture-panel {
    right: 8px;
    bottom: 8px;
  }

  .fixture-ribbon {
    left: 8px;
    bottom: 8px;
  }
}

@media (min-width: 840px) {
  .fixture-ribbon {
    left: calc(var(--nav-drawer-expanded) + 16px);
  }

  .app-shell.is-rail ~ .fixture-ribbon {
    left: calc(var(--nav-drawer-collapsed) + 16px);
  }
}
</style>
