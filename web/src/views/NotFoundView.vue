<template>
  <div class="not-found">
    <h1>404</h1>
    <p>页面不存在</p>
    <RouterLink to="/" class="back-link">返回首页</RouterLink>
  </div>
</template>

<script setup lang="ts">
import { RouterLink } from 'vue-router'
</script>

<style scoped>
.not-found {
  text-align: center;
  padding: 80px 0;
}

h1 {
  font-size: 72px;
  color: #c0c4cc;
  margin-bottom: 16px;
}

p {
  color: #606266;
  margin-bottom: 24px;
}

.back-link {
  color: #409eff;
  text-decoration: none;
  font-weight: 500;
}
</style>
