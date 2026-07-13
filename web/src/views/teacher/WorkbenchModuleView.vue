<template>
  <section class="module-view md-card" :aria-labelledby="headingId">
    <p class="eyebrow">教师工作台 / {{ title }}</p>
    <h2 :id="headingId">{{ title }}</h2>
    <p>{{ description }}</p>
    <div class="diagnostic" role="status">
      <strong>数据源尚未绑定</strong>
      <span>未认证只读演示不会显示或变更真实业务数据。配置 API 契约和授权后，此区域才会载入对应资源。</span>
    </div>
  </section>
</template>

<script setup lang="ts">
import { computed } from 'vue'

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
</style>
