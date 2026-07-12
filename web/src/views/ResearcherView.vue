<template>
  <RoleLayout
    icon="🔬"
    title="科研工作台"
    subtitle="长期工作环境、算力申请与软件配置"
    :tabs="tabs"
  >
    <div class="placeholder">
      <div class="placeholder-icon">💻</div>
      <h3 class="placeholder-title">工作环境</h3>
      <p class="placeholder-desc">申请容器或 VM 作为长期研究环境，按需申请 CPU/GPU，使用自然语言安装软件。</p>
    </div>
  </RoleLayout>
</template>

<script setup lang="ts">
import RoleLayout from '@/components/layout/RoleLayout.vue'

const tabs = [
  { path: '/researcher', label: '工作环境' },
  { path: '/researcher/resources', label: '资源申请' },
  { path: '/researcher/config', label: '软件配置' },
]
</script>

<style scoped>
.placeholder {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  min-height: 260px;
  text-align: center;
  color: var(--md-sys-color-on-surface-variant);
}

.placeholder-icon {
  width: 64px;
  height: 64px;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: var(--md-sys-shape-large);
  background: var(--md-sys-color-surface-variant);
  font-size: 32px;
  margin-bottom: 16px;
}

.placeholder-title {
  font: var(--md-sys-title-large);
  color: var(--md-sys-color-on-surface);
  margin-bottom: 8px;
}

.placeholder-desc {
  font: var(--md-sys-body-medium);
  max-width: 520px;
  line-height: 1.6;
}
</style>
