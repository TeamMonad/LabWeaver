<template>
  <RoleLayout
    icon="🎓"
    title="学生工作台"
    subtitle="加入实验、启动环境、提交结果并查看反馈"
    :tabs="tabs"
  >
    <div class="placeholder">
      <div class="placeholder-icon">🧪</div>
      <h3 class="placeholder-title">我的实验</h3>
      <p class="placeholder-desc">选择教师发布的实验，启动一致的容器或 VM 环境，提交后自动评测并查看证据。</p>
    </div>
  </RoleLayout>
</template>

<script setup lang="ts">
import RoleLayout from '@/components/layout/RoleLayout.vue'

const tabs = [
  { path: '/student', label: '我的实验' },
  { path: '/student/environments', label: '环境入口' },
  { path: '/student/results', label: '评测结果' },
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
