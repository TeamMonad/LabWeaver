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
  padding: var(--space-16) var(--space-8);
  text-align: center;
  min-height: 420px;
}

.not-found-code {
  font: 700 96px/1 var(--md-sys-font-family);
  color: var(--md-sys-color-primary);
  margin-bottom: var(--space-4);
}

.not-found-title {
  font: var(--md-sys-headline-small);
  color: var(--md-sys-color-on-surface);
  margin-bottom: var(--space-2);
}

.not-found-desc {
  font: var(--md-sys-body-large);
  color: var(--md-sys-color-on-surface-variant);
  margin-bottom: var(--space-8);
  max-width: 420px;
}

.home-btn {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  height: 40px;
  padding: 0 var(--space-6);
  border-radius: var(--md-sys-shape-full);
  background: var(--md-sys-color-primary);
  color: var(--md-sys-color-on-primary);
  font: var(--md-sys-label-large);
  transition: box-shadow var(--motion-duration-base) var(--motion-ease-out);
}

.home-btn:hover {
  box-shadow: var(--md-sys-elevation-1);
}
</style>
