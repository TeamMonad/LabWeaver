<template>
  <MockList
    title="我的实验"
    description="已加入的课程实验列表，点击即可启动一致的容器或 VM 环境。"
    :columns="[
      { key: 'name', label: '实验名称' },
      { key: 'environment', label: '环境类型' },
      { key: 'deadline', label: '截止时间' },
    ]"
    :rows="[
      { name: 'Linux 基础命令练习', environment: 'Container (Ubuntu 22.04)', deadline: '2026-07-20', status: '进行中', statusType: 'success' },
      { name: 'Python 数据结构实验', environment: 'VM (Fedora 38)', deadline: '2026-07-22', status: '未开始', statusType: 'neutral' },
      { name: 'OCI 容器镜像构建', environment: 'Container (BuildKit)', deadline: '2026-07-18', status: '已逾期', statusType: 'error' },
    ]"
  />
</template>

<script setup lang="ts">
import MockList from '@/components/common/MockList.vue'
</script>
