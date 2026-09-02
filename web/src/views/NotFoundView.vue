<template>
  <div class="not-found md-card">
    <div class="not-found-code">404</div>
    <h1 class="not-found-title">页面不存在</h1>
    <p class="not-found-desc">你访问的地址没有对应页面，请检查链接或返回首页。</p>
    <RouterLink to="/" class="home-btn"> 返回首页 </RouterLink>
  </div>
</template>

<script setup lang="ts">
import { RouterLink } from 'vue-router'
</script>

<style scoped>
.not-found {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  padding: 64px 32px;
  text-align: center;
  min-height: 420px;
}

.not-found-code {
  font: 700 96px/1 var(--md-sys-font-family);
  color: var(--md-sys-color-primary);
  margin-bottom: 16px;
}

.not-found-title {
  font: var(--md-sys-headline-small);
  color: var(--md-sys-color-on-surface);
  margin-bottom: 8px;
}

.not-found-desc {
  font: var(--md-sys-body-large);
  color: var(--md-sys-color-on-surface-variant);
  margin-bottom: 32px;
  max-width: 420px;
}

.home-btn {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  height: 40px;
  padding: 0 24px;
  border-radius: var(--md-sys-shape-full);
  background: var(--md-sys-color-primary);
  color: var(--md-sys-color-on-primary);
  font: var(--md-sys-label-large);
  transition: box-shadow 0.2s;
}

.home-btn:hover {
  box-shadow: var(--md-sys-elevation-1);
}
</style>
