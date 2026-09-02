<template>
  <aside
    class="navigation-drawer"
    :class="{
      'navigation-drawer--open': open,
      'navigation-drawer--rail': isRail,
      'navigation-drawer--modal': isModal,
    }"
    aria-label="应用导航"
  >
    <div class="drawer-header">
      <span class="drawer-title">LabWeaver</span>
      <button
        ref="closeButton"
        type="button"
        class="icon-button"
        aria-label="关闭导航"
        @click="$emit('close')"
      >
        <SvgIcon name="close" size="md" aria-label="关闭导航" />
      </button>
    </div>

    <nav class="drawer-nav" aria-label="角色入口">
      <RouterLink
        v-for="item in visibleRoleItems"
        :key="item.name"
        :to="item.path"
        class="drawer-item"
        :class="{ 'drawer-item--active': isActive(item.path) }"
        :aria-current="isActive(item.path) ? 'page' : undefined"
        @click="isModal && $emit('close')"
      >
        <SvgIcon :name="item.icon" size="md" :aria-label="item.label" />
        <span class="drawer-item__label">{{ item.label }}</span>
      </RouterLink>
      <p v-if="isAuthenticated && visibleRoleItems.length === 0" class="drawer-empty" role="note">
        当前账号未被授予任何工作台角色。
      </p>
    </nav>

    <div class="drawer-footer">
      <button
        v-if="!isModal"
        type="button"
        class="rail-toggle"
        :aria-label="isRail ? '展开导航' : '收起导航'"
        @click="$emit('toggle-rail')"
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
    @click="$emit('close')"
  />
</template>

<script setup lang="ts">
import { computed, ref, watch, nextTick } from 'vue'
import { RouterLink, useRoute } from 'vue-router'
import SvgIcon from '@/components/common/SvgIcon.vue'
import { useAuth } from '@/composables/useAuth'

const props = defineProps<{
  open: boolean
  rail?: boolean
}>()

const emit = defineEmits<{
  close: []
  toggleRail: []
}>()

const route = useRoute()
const isModal = computed(() => window.innerWidth < 840)
const isRail = computed(() => !isModal.value && props.rail)
const closeButton = ref<HTMLButtonElement | null>(null)

watch(
  () => props.open,
  (open) => {
    if (open && isModal.value) {
      void nextTick(() => closeButton.value?.focus())
    }
  }
)

const roleItems = [
  { name: 'teacher', role: 'teacher', path: '/teacher', label: '教师工作台', icon: 'school' as const },
  { name: 'student', role: 'student', path: '/student', label: '学生工作台', icon: 'person' as const },
  { name: 'researcher', role: 'researcher', path: '/researcher', label: '科研工作台', icon: 'science' as const },
  { name: 'admin', role: 'admin', path: '/admin', label: '管理工作台', icon: 'admin_panel_settings' as const },
]

const { user, isAuthenticated } = useAuth()
// Anonymous visitors keep all entries (they lead to the sign-in flow); a
// signed-in user only sees workbenches their OIDC roles authorize, so no
// dead links to a role_denied error page.
const visibleRoleItems = computed(() => {
  if (!isAuthenticated.value) return roleItems
  const roles = new Set((user.value?.profile?.roles as string[] | undefined) ?? [])
  return roleItems.filter((item) => roles.has(item.role))
})

const isActive = computed(() => (path: string) => route.path.startsWith(path))
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
  font: var(--md-sys-title-large);
  color: var(--md-sys-color-on-surface);
  overflow: hidden;
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
  flex-direction: column;
  gap: 4px;
  padding: 12px;
  flex: 1;
  min-height: 0;
  overflow-y: auto;
}

.drawer-item {
  display: flex;
  align-items: center;
  gap: 16px;
  height: 48px;
  min-height: 48px;
  flex-shrink: 0;
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
