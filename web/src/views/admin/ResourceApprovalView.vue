<template>
  <MockList
    title="资源审批"
    description="审批科研用户提交的 CPU/GPU 资源申请，查看配额和审计记录。"
    :columns="[
      { key: 'requester', label: '申请人' },
      { key: 'purpose', label: '用途' },
      { key: 'resource', label: '申请资源' },
    ]"
    :rows="[
      { requester: 'researcher-a', purpose: '大模型微调基准测试', resource: '1× A100 40GiB × 7 天', status: '待审批', statusType: 'warning' },
      { requester: 'researcher-b', purpose: '分子动力学模拟', resource: '16 vCPU / 64 GiB × 30 天', status: '已通过', statusType: 'success' },
      { requester: 'researcher-c', purpose: '数据集预处理', resource: '4 vCPU / 16 GiB × 14 天', status: '已拒绝', statusType: 'error' },
    ]"
  />
</template>

<script setup lang="ts">
import MockList from '@/components/common/MockList.vue'
</script>
