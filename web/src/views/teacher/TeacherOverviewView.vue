<template>
  <section class="overview">
    <div class="signal-grid" aria-label="教学闭环状态">
      <article v-for="signal in signals" :key="signal.label" class="signal-card md-card"><span>{{ signal.label }}</span><strong>{{ signal.value }}</strong><small>{{ signal.note }}</small></article>
    </div>
    <section class="table-section md-card">
      <header><div><h2>课程 / 实验</h2><p>{{ fixtureMode ? 'Fixture 数据仅用于界面验证，不代表真实发布、审批或评测结论。' : '课程、环境、审批与评测 API 尚未绑定；当前不会展示业务数据。' }}</p></div><button type="button" :disabled="!fixtureMode" @click="onlyAttention = !onlyAttention">{{ onlyAttention ? '显示全部' : '仅看需处理' }}</button></header>
      <div class="table-wrap"><table><thead><tr><th>实验</th><th>状态</th><th>待审批</th><th>环境</th><th>最近评测</th></tr></thead><tbody v-if="visibleExperiments.length"><tr v-for="experiment in visibleExperiments" :key="experiment.name"><td><strong>{{ experiment.name }}</strong><small>{{ experiment.course }}</small></td><td><span :class="['status', experiment.kind]">{{ experiment.status }}</span></td><td>{{ experiment.approvals }}</td><td>{{ experiment.environment }}</td><td>{{ experiment.evaluation }}</td></tr></tbody><tbody v-else><tr><td class="empty-state" colspan="5">{{ fixtureMode && onlyAttention ? '没有需要处理的 Fixture 实验。' : '未绑定数据源，未展示实验条目。' }}</td></tr></tbody></table></div>
    </section>
  </section>
</template>

<script setup lang="ts">
import { computed, ref } from 'vue'
import { FIXTURE_MODE_ENABLED } from '@/config'
const onlyAttention = ref(false)
const fixtureMode = FIXTURE_MODE_ENABLED
const signals = [{ label:'进行中实验', value:'—', note:'等待课程 API 绑定' },{ label:'待审批', value:'—', note:'等待审批 API 绑定' },{ label:'环境异常', value:'—', note:'等待环境事件绑定' },{ label:'最近评测', value:'—', note:'等待评测 API 绑定' }]
const experiments = fixtureMode ? [{ name:'Linux 系统实验',course:'Fixture：云原生实验',status:'需处理',kind:'warning',approvals:'—',environment:'未绑定',evaluation:'未绑定',attention:true },{ name:'KubeVirt VM 预检',course:'Fixture：云原生实验',status:'只读演示',kind:'neutral',approvals:'—',environment:'未绑定',evaluation:'未绑定',attention:false },{ name:'数据结构实验',course:'Fixture：程序设计基础',status:'只读演示',kind:'neutral',approvals:'—',environment:'未绑定',evaluation:'未绑定',attention:false }] : []
const visibleExperiments = computed(() => onlyAttention.value ? experiments.filter((item) => item.attention) : experiments)
</script>

<style scoped>
.overview { display:grid; gap:20px; }.signal-grid { display:grid; grid-template-columns:repeat(4,minmax(0,1fr)); gap:16px; }.signal-card { display:grid; gap:7px; padding:18px; }.signal-card span,.signal-card small { color:var(--md-sys-color-on-surface-variant); font:var(--md-sys-body-small); }.signal-card strong { font:var(--md-sys-headline-small); }.table-section { overflow:hidden; }.table-section header { display:flex; justify-content:space-between; gap:16px; padding:20px 20px 16px; }.table-section h2 { font:var(--md-sys-title-large); }.table-section p { margin-top:4px; color:var(--md-sys-color-on-surface-variant); font:var(--md-sys-body-small); }.table-section button { align-self:flex-start; border:1px solid var(--md-sys-color-outline); border-radius:var(--md-sys-shape-full); padding:8px 12px; background:#fff; color:var(--md-sys-color-primary); font:var(--md-sys-label-large); cursor:pointer; }.table-section button:disabled { cursor:not-allowed; opacity:.55; }.table-wrap { overflow-x:auto; } table { width:100%; border-collapse:collapse; min-width:680px; } th,td { padding:14px 20px; text-align:left; border-top:1px solid var(--md-sys-color-outline-variant); } th { color:var(--md-sys-color-on-surface-variant); font:var(--md-sys-label-medium); background:#f8f9fa; } td { font:var(--md-sys-body-medium); } td small { display:block; margin-top:3px; color:var(--md-sys-color-on-surface-variant); font:var(--md-sys-body-small); }.empty-state { color:var(--md-sys-color-on-surface-variant); text-align:center; }.status { display:inline-flex; border-radius:var(--md-sys-shape-small); padding:4px 8px; font:var(--md-sys-label-medium); }.warning { background:#fff4e5; color:#8a6d1f; }.neutral { background:#f1f3f4; color:#5f6368; } @media (max-width:760px) { .signal-grid { grid-template-columns:repeat(2,minmax(0,1fr)); }.table-section header { display:block; }.table-section button { margin-top:12px; } }
</style>
