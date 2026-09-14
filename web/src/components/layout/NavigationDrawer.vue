<template>
  <aside
    class="navigation-drawer"
    :class="{
      'navigation-drawer--open': open,
      'navigation-drawer--rail': isRail,
      'navigation-drawer--modal': isModal,
    }"
    :aria-hidden="isModal && !open ? 'true' : undefined"
    :inert="isModal && !open"
    aria-label="应用导航"
  >
    <div class="drawer-header">
      <span class="drawer-title">LabWeaver</span>
      <button
        ref="closeButton"
        type="button"
        class="icon-button"
        aria-label="关闭导航"
        @click="emit('close')"
      >
        <SvgIcon name="close" size="md" aria-label="关闭导航" />
      </button>
    </div>

    <nav class="drawer-nav" aria-label="任务导航">
      <section
        v-for="group in visibleGroups"
        :key="group.id"
        class="nav-group"
        :data-nav-group="group.id"
        :aria-labelledby="`nav-group-${group.id}`"
      >
        <h2
          :id="`nav-group-${group.id}`"
          class="nav-section-title"
          :class="{ 'nav-section-title--visually-hidden': isRail }"
        >
          {{ group.label }}
        </h2>
        <div class="nav-group-items">
          <RouterLink
            v-for="item in group.items"
            :key="item.id"
            :to="navigationTarget(item, selectedProjectId)"
            class="drawer-item"
            :class="{ 'drawer-item--active': isActive(item.path) }"
            :aria-current="isActive(item.path) ? 'page' : undefined"
            :aria-label="isRail ? `${group.label}：${item.label}` : undefined"
            :title="isRail ? `${group.label}：${item.label}` : undefined"
            @click="isModal && emit('close')"
          >
            <SvgIcon :name="item.icon" size="md" aria-hidden="true" />
            <span class="drawer-item__label">{{ item.label }}</span>
          </RouterLink>
        </div>
      </section>

      <p v-if="!isAuthenticated" class="drawer-empty" role="note">
        登录后显示可用任务。
      </p>
      <p v-else-if="visibleGroups.length === 0" class="drawer-empty" role="note">
        当前账号未授予任何可用任务。
      </p>
    </nav>

    <div class="drawer-footer">
      <button
        v-if="!isModal"
        type="button"
        class="rail-toggle"
        :aria-label="isRail ? '展开导航' : '收起导航'"
        @click="emit('toggleRail')"
      >
        <SvgIcon :name="isRail ? 'chevron_right' : 'chevron_left'" size="md" aria-hidden="true" />
        <span v-if="!isRail" class="rail-toggle__label">收起</span>
      </button>
    </div>
  </aside>

  <div
    v-if="isModal && open"
    class="drawer-scrim"
    aria-hidden="true"
    @click="emit('close')"
  />
</template>

<script setup lang="ts">
import { computed, nextTick, onMounted, onScopeDispose, ref, watch } from 'vue'
import { RouterLink, useRoute } from 'vue-router'
import SvgIcon from '@/components/common/SvgIcon.vue'
import { useAuth } from '@/composables/useAuth'
import { useProjects } from '@/composables/useProjects'
import { navigationGroupsForRoles, navigationTarget, rolesFromProfile } from '@/utils/navigation'

const props = defineProps<{
  open: boolean
  rail?: boolean
}>()

const emit = defineEmits<{
  close: []
  toggleRail: []
}>()

const route = useRoute()
const auth = useAuth()
const projects = useProjects()
const closeButton = ref<HTMLButtonElement | null>(null)
const viewportWidth = ref(typeof window === 'undefined' ? 1280 : window.innerWidth)
const previouslyFocused = ref<HTMLElement | null>(null)

const isModal = computed(() => viewportWidth.value < 840)
const isRail = computed(() => !isModal.value && Boolean(props.rail))
const isAuthenticated = computed(() => auth.isAuthenticated.value)
const roles = computed(() => rolesFromProfile(auth.user.value?.profile))
const visibleGroups = computed(() => isAuthenticated.value ? navigationGroupsForRoles(roles.value) : [])
const selectedProjectId = computed(() => projects.selectedProjectId)

watch(
  () => props.open,
  (open) => {
    if (open && isModal.value) {
      previouslyFocused.value = document.activeElement instanceof HTMLElement ? document.activeElement : null
      void nextTick(() => closeButton.value?.focus())
    } else if (!open && isModal.value) {
      void nextTick(() => previouslyFocused.value?.focus())
      previouslyFocused.value = null
    }
  },
)

function isActive(path: string) {
  return route.path === path || route.path.startsWith(`${path}/`)
}

function updateViewportWidth() {
  viewportWidth.value = window.innerWidth
}

function handleKeydown(event: KeyboardEvent) {
  if (event.key === 'Escape' && isModal.value && props.open) {
    event.preventDefault()
    emit('close')
  }
}

onMounted(() => {
  window.addEventListener('resize', updateViewportWidth)
  document.addEventListener('keydown', handleKeydown)
})

onScopeDispose(() => {
  window.removeEventListener('resize', updateViewportWidth)
  document.removeEventListener('keydown', handleKeydown)
})
</script>

<style scoped>
.navigation-drawer {
  position: fixed;
  top: 0;
  left: 0;
  z-index: 1200;
  display: flex;
  flex-direction: column;
  width: var(--nav-drawer-expanded);
  height: 100%;
  background: var(--md-sys-color-surface);
  border-right: 1px solid var(--md-sys-color-outline-variant);
  transform: translateX(-100%);
  transition: transform 0.25s ease, width 0.25s ease;
}

.navigation-drawer--open {
  transform: translateX(0);
}

.navigation-drawer--rail {
  width: var(--nav-drawer-collapsed);
}

.drawer-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  height: var(--app-top-bar-height);
  padding: 0 16px;
  border-bottom: 1px solid var(--md-sys-color-outline-variant);
}

.drawer-title {
  overflow: hidden;
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-title-large);
  text-overflow: ellipsis;
  white-space: nowrap;
}

.icon-button {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 48px;
  height: 48px;
  padding: 0;
  border: none;
  border-radius: var(--md-sys-shape-full);
  background: transparent;
  color: var(--md-sys-color-on-surface-variant);
  cursor: pointer;
}

.drawer-nav {
  display: flex;
  flex: 1;
  min-height: 0;
  flex-direction: column;
  gap: 12px;
  overflow-y: auto;
  padding: 12px;
}

.nav-group {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.nav-group + .nav-group {
  padding-top: 8px;
  border-top: 1px solid var(--md-sys-color-outline-variant);
}

.nav-section-title {
  padding: 4px 12px;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-label-small);
  font-size: 11px;
  font-weight: 600;
  letter-spacing: 0.5px;
}

.nav-group-items {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.drawer-item {
  display: flex;
  align-items: center;
  gap: 16px;
  min-height: 44px;
  padding: 0 16px;
  border-radius: var(--md-sys-shape-full);
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-label-large);
  text-decoration: none;
  overflow: hidden;
}

.drawer-item:hover {
  background: var(--md-sys-color-surface-container-highest);
  color: var(--md-sys-color-on-surface);
}

.drawer-item:focus-visible,
.icon-button:focus-visible,
.rail-toggle:focus-visible {
  outline: 2px solid var(--md-sys-color-primary);
  outline-offset: 2px;
}

.drawer-item--active {
  background: var(--md-sys-color-secondary-container);
  color: var(--md-sys-color-on-secondary-container);
}

.drawer-item__label {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.drawer-empty {
  margin: 8px;
  padding: 12px;
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-surface-container);
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-small);
}

.navigation-drawer--rail .drawer-item {
  justify-content: center;
  padding: 0;
}

.navigation-drawer--rail .drawer-item__label {
  display: none;
}

.nav-section-title--visually-hidden {
  position: absolute;
  width: 1px;
  height: 1px;
  padding: 0;
  margin: -1px;
  overflow: hidden;
  clip: rect(0, 0, 0, 0);
  white-space: nowrap;
  border: 0;
}

.drawer-footer {
  padding: 12px;
  border-top: 1px solid var(--md-sys-color-outline-variant);
}

.rail-toggle {
  display: flex;
  align-items: center;
  gap: 12px;
  width: 100%;
  height: 40px;
  padding: 0 12px;
  border: none;
  border-radius: var(--md-sys-shape-full);
  background: transparent;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-label-large);
  cursor: pointer;
}

.rail-toggle:hover {
  background: var(--md-sys-color-surface-container-highest);
}

.navigation-drawer--rail .rail-toggle {
  justify-content: center;
  padding: 0;
}

.navigation-drawer--rail .rail-toggle__label {
  display: none;
}

.drawer-scrim {
  position: fixed;
  inset: 0;
  z-index: 1150;
  background: var(--md-sys-color-scrim);
}

@media (min-width: 840px) {
  .navigation-drawer {
    position: static;
    top: auto;
    left: auto;
    grid-row: 3;
    grid-column: 1;
    transform: none;
  }

  .drawer-header {
    display: none;
  }

  .drawer-scrim {
    display: none;
  }
}

@media (prefers-reduced-motion: reduce) {
  .navigation-drawer {
    transition: none;
  }
}
</style>
