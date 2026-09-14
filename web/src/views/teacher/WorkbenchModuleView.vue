<template>
  <section class="module-view md-card" :aria-labelledby="headingId">
    <p class="eyebrow">教师工作台 / {{ title }}</p>
    <h2 :id="headingId">{{ title }}</h2>
    <p>{{ description }}</p>
    <div class="diagnostic" role="status">
      <strong>当前没有可用数据</strong>
      <span>请返回实验总览继续准备材料；如果问题持续，请联系管理员。</span>
      <RouterLink to="/teacher/overview" class="diagnostic-action">返回实验总览</RouterLink>
    </div>
  </section>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { RouterLink } from 'vue-router'

const props = defineProps<{ title: string; description: string }>()
const headingId = computed(() => `teacher-${props.title}-heading`)
</script>

<style scoped>
.module-view { display:grid; gap:14px; padding:28px; min-height:300px; align-content:start; }
.eyebrow { color:var(--md-sys-color-on-surface-variant); font:var(--md-sys-label-medium); }
h2 { font:var(--md-sys-title-large); }
.module-view > p:not(.eyebrow) { max-width:640px; color:var(--md-sys-color-on-surface-variant); font:var(--md-sys-body-medium); line-height:1.6; }
.diagnostic { display:grid; gap:4px; max-width:680px; margin-top:10px; padding:14px; border-radius:var(--md-sys-shape-medium); background:var(--md-sys-color-surface-variant); color:var(--md-sys-color-on-surface-variant); font:var(--md-sys-body-small); line-height:1.5; }
.diagnostic strong { color:var(--md-sys-color-on-surface); font:var(--md-sys-label-large); }
.diagnostic-action { width: fit-content; color:var(--md-sys-color-primary); font:var(--md-sys-label-large); text-decoration: none; }
</style>
