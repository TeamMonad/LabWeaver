<template>
  <div class="role-view">
    <h2>🎓 学生工作台</h2>
    <nav class="section-nav">
      <RouterLink to="/student">我的实验</RouterLink>
      <RouterLink to="/student/environments">环境入口</RouterLink>
      <RouterLink to="/student/results">评测结果</RouterLink>
    </nav>
    <div class="placeholder">
      <p>学生实验功能占位页</p>
      <p class="hint">后续接入：实验列表、启动环境、提交与结果时间线</p>
    </div>
  </div>
</template>

<script setup lang="ts">
import { RouterLink } from 'vue-router'
</script>

<style scoped>
.role-view {
  background: #fff;
  border-radius: 12px;
  padding: 32px;
  box-shadow: 0 2px 12px rgba(0, 0, 0, 0.06);
}

h2 {
  margin-bottom: 20px;
  font-size: 24px;
}

.section-nav {
  display: flex;
  gap: 16px;
  margin-bottom: 24px;
  padding-bottom: 16px;
  border-bottom: 1px solid #e4e7ed;
}

.section-nav a {
  color: #606266;
  text-decoration: none;
  font-weight: 500;
}

.section-nav a:hover {
  color: #409eff;
}

.placeholder {
  padding: 48px;
  text-align: center;
  color: #909399;
  background: #f5f7fa;
  border-radius: 8px;
}

.hint {
  margin-top: 8px;
  font-size: 13px;
}
</style>
