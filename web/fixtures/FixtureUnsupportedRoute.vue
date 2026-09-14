<template>
  <section class="fixture-unsupported md-card" aria-labelledby="fixture-unsupported-heading">
    <p class="fixture-kicker">Fixture preview boundary</p>
    <h2 id="fixture-unsupported-heading">此任务未纳入本地预览</h2>
    <p>当前场景画廊只接入了可复用的真实页面和读取接口；该任务不会显示伪造的成功状态。</p>
    <p class="fixture-path">当前路径：{{ route.path }}</p>
</section>
</template>

<script setup lang="ts">
import { useRoute } from 'vue-router'

const route = useRoute()
</script>

<style scoped>
.fixture-unsupported {
  display: grid;
  gap: 12px;
  max-width: 680px;
  padding: 28px;
}

.fixture-kicker {
  color: var(--md-sys-color-primary);
  font: var(--md-sys-label-small);
  letter-spacing: .05em;
  text-transform: uppercase;
}

.fixture-unsupported h2 {
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-headline-small);
}

.fixture-unsupported p {
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-medium);
  line-height: 1.5;
}

.fixture-path {
  overflow-wrap: anywhere;
  font-family: ui-monospace, SFMono-Regular, Consolas, monospace !important;
}
</style>
