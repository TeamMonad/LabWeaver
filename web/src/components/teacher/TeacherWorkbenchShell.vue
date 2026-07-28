<template>
  <section class="teacher-shell">
    <button
      class="mobile-menu"
      type="button"
      aria-label="切换工作台导航"
      @click="drawerOpen = !drawerOpen"
    >
      <SvgIcon name="menu" size="md" aria-hidden="true" />
    </button>
    <aside class="sidebar" :class="{ 'sidebar-open': drawerOpen }" aria-label="教师工作台导航">
      <div class="sidebar-heading">
        <span>教师工作台</span>
      </div>
      <nav class="module-nav" aria-label="工作台模块">
        <RouterLink to="/teacher/overview" active-class="module-active">实验总览</RouterLink>
        <RouterLink to="/teacher/labs" active-class="module-active">实验</RouterLink>
        <RouterLink to="/teacher/materials" active-class="module-active">材料</RouterLink>
        <RouterLink to="/teacher/environments" active-class="module-active">环境</RouterLink>
        <RouterLink to="/teacher/approvals" active-class="module-active">候选审批</RouterLink>
      </nav>
      <section class="resource-tree" aria-label="课程与实验资源树">
        <h2>课程与实验</h2>
        <p class="resource-empty">课程与实验 API 尚未绑定，未展示资源数据。</p>
      </section>
    </aside>
    <div class="workspace">
      <header class="workspace-header">
        <div>
          <p class="breadcrumb">教师 / 实验工作台</p>
          <h1>{{ workspaceTitle }}</h1>
        </div>
        <button class="primary-action" type="button" @click="showCreateNotice = true">+ 新建实验</button>
      </header>
      <p v-if="showCreateNotice" class="blocking-notice" role="status">实验创建服务尚未绑定。此入口不会创建任何业务数据。</p>
      <RouterView />
    </div>
  </section>
</template>

<script setup lang="ts">
import { computed, ref } from 'vue'
import { RouterLink, RouterView, useRoute } from 'vue-router'
import SvgIcon from '@/components/common/SvgIcon.vue'

const route = useRoute()
const workspaceTitle = computed(() => (route.meta.title as string | undefined) ?? '实验总览')

const drawerOpen = ref(false)
const showCreateNotice = ref(false)
</script>

<style scoped>
.teacher-shell {
  display: grid;
  grid-template-columns: 248px minmax(0, 1fr);
  min-height: calc(100vh - var(--app-top-bar-height));
  margin: -24px -32px -48px;
}

.sidebar {
  background: var(--md-sys-color-surface);
  border-right: 1px solid var(--md-sys-color-outline-variant);
  padding: 24px 14px;
}

.sidebar-heading {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 0 10px 20px;
  font: var(--md-sys-title-medium);
  color: var(--md-sys-color-on-surface);
}

.module-nav {
  display: grid;
  gap: 4px;
  padding-bottom: 20px;
  border-bottom: 1px solid var(--md-sys-color-outline-variant);
}

.module-nav a {
  padding: 10px 12px;
  border-radius: var(--md-sys-shape-medium);
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-label-large);
  text-decoration: none;
}

.module-nav a:hover,
.module-active {
  background: var(--md-sys-color-primary-container);
  color: var(--md-sys-color-on-primary-container);
}

.resource-tree {
  padding: 20px 10px;
}

.resource-tree h2 {
  font: var(--md-sys-label-large);
  margin-bottom: 12px;
  color: var(--md-sys-color-on-surface);
}

.resource-empty {
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-small);
  line-height: 1.5;
}

.workspace {
  padding: 28px 32px 48px;
  min-width: 0;
}

.workspace-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 20px;
  margin-bottom: 16px;
}

.breadcrumb {
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-small);
  margin-bottom: 4px;
}

h1 {
  font: var(--md-sys-headline-small);
  color: var(--md-sys-color-on-surface);
}

.primary-action {
  border: 0;
  border-radius: var(--md-sys-shape-full);
  padding: 11px 18px;
  background: var(--md-sys-color-primary);
  color: var(--md-sys-color-on-primary);
  font: var(--md-sys-label-large);
  cursor: pointer;
  white-space: nowrap;
}

.blocking-notice {
  margin-bottom: 16px;
  padding: 10px 12px;
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-error-container);
  color: var(--md-sys-color-on-error-container);
  font: var(--md-sys-body-small);
}

.mobile-menu {
  display: none;
}

@media (max-width: 760px) {
  .teacher-shell {
    display: block;
    margin: -24px -16px -48px;
  }

  .mobile-menu {
    display: flex;
    align-items: center;
    justify-content: center;
    position: fixed;
    right: 12px;
    bottom: 16px;
    z-index: 20;
    width: 44px;
    height: 44px;
    border: 0;
    border-radius: 50%;
    background: var(--md-sys-color-primary);
    color: var(--md-sys-color-on-primary);
    box-shadow: var(--md-sys-elevation-2);
    cursor: pointer;
  }

  .sidebar {
    display: none;
    position: fixed;
    inset: var(--app-top-bar-height) auto 0 0;
    z-index: 10;
    width: 280px;
    box-shadow: var(--md-sys-elevation-3);
  }

  .sidebar-open {
    display: block;
  }

  .workspace {
    /* Leave room for the fixed mobile module-nav toggle so it never overlaps
       the last lines of content on small screens. */
    padding: 24px 16px 88px;
  }

  .workspace-header {
    align-items: flex-start;
  }
}

@media (prefers-reduced-motion: reduce) {
  .module-nav a {
    transition: none;
  }
}
</style>
