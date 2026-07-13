<template>
  <MockList
    title="工作环境"
    description="长期工作容器或 VM 列表，支持启动、停止、续期和销毁。"
    :columns="[
      { key: 'name', label: '工作区' },
      { key: 'type', label: '类型' },
      { key: 'resources', label: '资源配置' },
    ]"
    :rows="[
      { name: 'ml-benchmark-01', type: 'VM (KubeVirt)', resources: '8 vCPU / 32 GiB / 1×GPU', status: '运行中', statusType: 'success' },
      { name: 'data-pipeline-dev', type: 'Container', resources: '4 vCPU / 16 GiB', status: '已停止', statusType: 'neutral' },
      { name: 'hpc-simulation', type: 'VM (KubeVirt)', resources: '16 vCPU / 64 GiB', status: '创建中', statusType: 'warning' },
    ]"
  />
</template>

<script setup lang="ts">
import MockList from '@/components/common/MockList.vue'
</script>
