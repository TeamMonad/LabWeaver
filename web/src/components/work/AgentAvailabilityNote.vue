<template>
  <div
    class="agent-note"
    :class="`agent-note--${state.kind}`"
    role="status"
    data-testid="agent-availability"
    :data-agent-state="state.kind"
  >
    <SvgIcon :name="icon" size="sm" aria-hidden="true" />
    <span v-if="state.kind === 'configured'">Agent 可用 · 模型 <code>{{ state.model }}</code></span>
    <span v-else-if="state.kind === 'unconfigured' || state.kind === 'unavailable'">
      Agent 已关闭：{{ agentCapabilityReason(state) }}。Agent 生成与配置不可用，手工创建和连接 Work 不受影响。
      <RouterLink v-if="projectId" :to="{ path: '/researcher/ai-policy', query: { projectId } }">项目 AI 设置</RouterLink>
    </span>
    <span v-else-if="state.kind === 'unreadable'">
      暂时无法确认 Agent 是否可用：{{ state.diagnostic.message }} 读取失败不代表项目没有模型；手工创建和连接 Work 不受影响。
      <details class="agent-note__details">
        <summary>诊断信息</summary>
        <code>{{ state.diagnostic.code }}</code>
        <code v-if="state.diagnostic.requestId">{{ state.diagnostic.requestId }}</code>
      </details>
    </span>
    <span v-else>正在确认 Agent 是否可用…</span>
  </div>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { RouterLink } from 'vue-router'
import SvgIcon from '@/components/common/SvgIcon.vue'
import { agentCapabilityReason, type AgentCapabilityState } from '@/composables/useProjectAgentCapability'

const props = defineProps<{ state: AgentCapabilityState; projectId?: string | null }>()
const icon = computed(() => props.state.kind === 'configured' ? 'smart_toy' : props.state.kind === 'loading' || props.state.kind === 'idle' ? 'hourglass_empty' : 'info')
</script>

<style scoped>
.agent-note { display: inline-flex; align-items: flex-start; gap: var(--space-2); max-width: 460px; padding: var(--space-2) var(--space-3); border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.agent-note--configured { background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); }
.agent-note a { color: var(--md-sys-color-primary); }
.agent-note code { font: var(--md-sys-label-medium); overflow-wrap: anywhere; }
.agent-note__details { margin-top: var(--space-1); }
.agent-note__details code { display: block; }
</style>
