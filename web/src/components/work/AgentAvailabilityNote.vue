<template>
  <div
    class="agent-note"
    :class="`agent-note--${state.kind}`"
    role="status"
    data-testid="agent-availability"
  >
    <SvgIcon :name="icon" size="sm" aria-hidden="true" />
    <span v-if="state.kind === 'available'">Agent 可用 · 模型 <code>{{ state.model }}</code></span>
    <span v-else-if="state.kind === 'unconfigured'">
      未配置模型：Agent 生成与配置不可用，手工创建和连接 Work 不受影响。
      <RouterLink v-if="projectId" :to="{ path: '/researcher/ai-policy', query: { projectId } }">项目 AI 设置</RouterLink>
    </span>
    <span v-else-if="state.kind === 'unknown'">暂时无法确认 Agent 是否可用（{{ state.code }}），手工创建和连接 Work 不受影响。</span>
    <span v-else>正在确认 Agent 是否可用…</span>
  </div>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { RouterLink } from 'vue-router'
import SvgIcon from '@/components/common/SvgIcon.vue'
import type { AgentAvailability } from '@/composables/useAgentAvailability'

const props = defineProps<{ state: AgentAvailability; projectId?: string | null }>()
const icon = computed(() => props.state.kind === 'available' ? 'smart_toy' : props.state.kind === 'loading' || props.state.kind === 'idle' ? 'hourglass_empty' : 'info')
</script>

<style scoped>
.agent-note { display: inline-flex; align-items: flex-start; gap: 8px; max-width: 460px; padding: 8px 12px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.agent-note--available { background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); }
.agent-note a { color: var(--md-sys-color-primary); }
.agent-note code { font: var(--md-sys-label-medium); }
</style>
