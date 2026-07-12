<template>
  <RoleLayout
    icon="👨‍🏫"
    title="教师工作台"
    subtitle="创建实验、审核 Agent 生成物、发布实验包"
    :tabs="tabs"
  >
    <div class="placeholder">
      <div class="placeholder-icon">📝</div>
      <h3 class="placeholder-title">实验管理</h3>
      <p class="placeholder-desc">上传题面材料、查看 Agent 生成的环境/评测 YAML、审批发布后学生即可使用。</p>
    </div>
  </RoleLayout>
</template>

<script setup lang="ts">
import RoleLayout from '@/components/layout/RoleLayout.vue'

const tabs = [
  { path: '/teacher', label: '实验列表' },
  { path: '/teacher/materials', label: '材料上传' },
  { path: '/teacher/approvals', label: '待审批' },
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
