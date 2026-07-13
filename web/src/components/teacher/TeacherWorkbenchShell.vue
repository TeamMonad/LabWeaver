<template>
  <section class="teacher-shell">
    <button class="mobile-menu" type="button" aria-label="切换工作台导航" @click="drawerOpen = !drawerOpen">☰</button>
    <aside class="sidebar" :class="{ 'sidebar-open': drawerOpen }" aria-label="教师工作台导航">
      <div class="sidebar-heading"><span>教师工作台</span><small>只读演示</small></div>
      <nav class="module-nav" aria-label="工作台模块">
        <RouterLink to="/teacher/overview" active-class="module-active">实验总览</RouterLink>
        <RouterLink to="/teacher/labs" active-class="module-active">实验</RouterLink>
        <RouterLink to="/teacher/environments" active-class="module-active">环境</RouterLink>
        <RouterLink to="/teacher/evaluations" active-class="module-active">评测</RouterLink>
        <RouterLink to="/teacher/resources" active-class="module-active">资源</RouterLink>
      </nav>
      <section class="resource-tree" aria-label="课程与实验资源树">
        <h2>课程与实验</h2>
        <template v-if="fixtureMode">
          <details open><summary>Fixture：云原生实验</summary><RouterLink to="/teacher/labs">KubeVirt VM 预检</RouterLink><RouterLink to="/teacher/labs">Linux 系统实验</RouterLink></details>
          <details><summary>Fixture：程序设计基础</summary><RouterLink to="/teacher/labs">数据结构实验</RouterLink></details>
        </template>
        <p v-else class="resource-empty">课程与实验 API 尚未绑定，未展示资源数据。</p>
      </section>
    </aside>
    <div class="workspace">
      <header class="workspace-header">
        <div><p class="breadcrumb">教师 / 实验工作台</p><h1>实验总览</h1></div>
        <button class="primary-action" type="button" @click="showCreateNotice = true">+ 新建实验</button>
      </header>
      <p v-if="showCreateNotice" class="blocking-notice" role="status">实验创建服务尚未绑定。此入口不会创建任何业务数据。</p>
      <p class="read-only-notice">未认证只读演示：不显示真实用户、平台健康或后端业务数据。</p>
      <RouterView />
    </div>
  </section>
</template>

<script setup lang="ts">
import { ref } from 'vue'
import { RouterLink, RouterView } from 'vue-router'
import { FIXTURE_MODE_ENABLED } from '@/config'

const drawerOpen = ref(false)
const showCreateNotice = ref(false)
const fixtureMode = FIXTURE_MODE_ENABLED
</script>

<style scoped>
.teacher-shell { display:grid; grid-template-columns:248px minmax(0,1fr); min-height:calc(100vh - var(--app-header-height)); margin:-24px -32px -48px; }
.sidebar { background:#fff; border-right:1px solid var(--md-sys-color-outline-variant); padding:24px 14px; }
.sidebar-heading { display:flex; justify-content:space-between; align-items:center; padding:0 10px 20px; font:var(--md-sys-title-medium); }
.sidebar-heading small { color:var(--md-sys-color-on-surface-variant); font:var(--md-sys-label-medium); }
.module-nav { display:grid; gap:4px; padding-bottom:20px; border-bottom:1px solid var(--md-sys-color-outline-variant); }
.module-nav a { padding:10px 12px; border-radius:var(--md-sys-shape-medium); color:var(--md-sys-color-on-surface-variant); font:var(--md-sys-label-large); }
.module-nav a:hover,.module-active { background:var(--md-sys-color-primary-container); color:var(--md-sys-color-on-primary-container); }
.resource-tree { padding:20px 10px; }
.resource-tree h2 { font:var(--md-sys-label-large); margin-bottom:12px; }
.resource-tree details { margin:10px 0; color:var(--md-sys-color-on-surface-variant); }
.resource-tree summary { cursor:pointer; font:var(--md-sys-label-large); }
.resource-tree a { display:block; padding:7px 0 2px 16px; font:var(--md-sys-body-small); }
.resource-empty { color:var(--md-sys-color-on-surface-variant); font:var(--md-sys-body-small); line-height:1.5; }
.workspace { padding:28px 32px 48px; min-width:0; }
.workspace-header { display:flex; justify-content:space-between; align-items:center; gap:20px; margin-bottom:16px; }
.breadcrumb { color:var(--md-sys-color-on-surface-variant); font:var(--md-sys-body-small); margin-bottom:4px; }
h1 { font:var(--md-sys-headline-small); }
.primary-action { border:0; border-radius:var(--md-sys-shape-full); padding:11px 18px; background:var(--md-sys-color-primary); color:#fff; font:var(--md-sys-label-large); cursor:pointer; }
.read-only-notice,.blocking-notice { margin-bottom:16px; padding:10px 12px; border-radius:var(--md-sys-shape-medium); font:var(--md-sys-body-small); }
.read-only-notice { background:#f1f3f4; color:var(--md-sys-color-on-surface-variant); }
.blocking-notice { background:#fce8e6; color:#9f2b22; }
.mobile-menu { display:none; }
@media (max-width: 760px) { .teacher-shell { display:block; margin:-24px -16px -48px; } .mobile-menu { display:block; position:fixed; left:12px; bottom:16px; z-index:20; width:44px; height:44px; border:0; border-radius:50%; background:var(--md-sys-color-primary); color:#fff; box-shadow:var(--md-sys-elevation-2); } .sidebar { display:none; position:fixed; inset:var(--app-header-height) auto 0 0; z-index:10; width:280px; box-shadow:var(--md-sys-elevation-3); } .sidebar-open { display:block; } .workspace { padding:24px 16px 56px; } .workspace-header { align-items:flex-start; } .primary-action { white-space:nowrap; } }
</style>
