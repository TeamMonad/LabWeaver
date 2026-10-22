<template>
  <section class="project-recovery md-card" role="alert" aria-labelledby="project-recovery-title" data-testid="project-context-recovery">
    <div class="project-recovery__icon" aria-hidden="true">
      <SvgIcon name="link_off" size="xl" />
    </div>
    <div class="project-recovery__body">
      <h1 id="project-recovery-title">{{ title }}</h1>
      <p class="project-recovery__reason">{{ reason }}</p>
      <p class="project-recovery__next">{{ nextStep }}</p>
      <p class="project-recovery__note">为避免误操作，本页不会显示其他项目的数据。</p>

      <div class="project-recovery__actions">
        <button
          v-if="state.fallback"
          ref="primaryAction"
          type="button"
          class="filled-button"
          @click="switchTo(state.fallback.id)"
        >
          <SvgIcon name="swap_horiz" size="sm" aria-hidden="true" />
          <span>切换到「{{ state.fallback.name }}」</span>
        </button>
        <RouterLink
          :to="{ path: '/researcher/workspaces' }"
          class="outlined-button"
        >
          查看我的项目
        </RouterLink>
        <RouterLink :to="{ path: '/' }" class="text-button">
          返回进行中
        </RouterLink>
      </div>

      <DiagnosticBanner
        v-if="state.diagnostic"
        class="project-recovery__diagnostic"
        :diagnostic="{ ...state.diagnostic, nextStep: undefined, retryable: false }"
        severity="warning"
      />
      <details class="project-recovery__link">
        <summary>链接中的项目标识</summary>
        <code>{{ state.projectId }}</code>
      </details>
    </div>
  </section>
</template>

<script setup lang="ts">
import { computed, nextTick, onMounted, ref } from 'vue'
import { RouterLink, useRoute, useRouter } from 'vue-router'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import SvgIcon from '@/components/common/SvgIcon.vue'
import { projectSwitchLocation, type ProjectContextState } from '@/composables/useProjectContext'
import { useProjects } from '@/composables/useProjects'
import { projectScopedBasePath } from '@/utils/navigation'

const props = defineProps<{
  state: Extract<ProjectContextState, { kind: 'unavailable' }>
}>()

const route = useRoute()
const router = useRouter()
const projects = useProjects()
const primaryAction = ref<HTMLButtonElement | null>(null)

const title = computed(() => props.state.reason === 'checking' ? '正在确认链接中的项目…' : '无法打开链接中的项目')
const reason = computed(() => {
  switch (props.state.reason) {
    case 'checking':
      return '当前账号的项目列表中没有这个项目，正在向平台确认原因。'
    case 'not-accessible':
      // 401/403/404 cannot tell apart "missing" from "not allowed", so the copy
      // never claims the project exists.
      return '项目不存在或你无权访问。'
    case 'not-listed':
      return '这个项目不在你的项目列表中，不能在这里打开。'
    case 'archived-or-hidden':
      return '这个项目已归档，不能继续在这里操作。'
    default:
      return props.state.diagnostic?.message ?? '暂时无法确认这个项目能否访问。'
  }
})
const nextStep = computed(() => {
  if (props.state.reason === 'not-accessible' || props.state.reason === 'not-listed') {
    return props.state.fallback
      ? '请切换到你可以访问的项目；如果你认为应该能访问这个项目，请联系项目 Owner 或管理员确认。'
      : '你目前没有可访问的项目。如果你认为应该能访问这个项目，请联系项目 Owner 或管理员确认。'
  }
  if (props.state.fallback) return '请切换到你可以访问的项目，或回到项目列表重新选择。'
  return '你目前没有可访问的项目。请在“项目与工作空间”创建项目，或联系项目 Owner 邀请你加入。'
})

function switchTo(projectId: string) {
  projects.select(projectId)
  const path = projectScopedBasePath(route.path) ?? route.path
  void router.replace(projectSwitchLocation(path, route.query, projectId))
}

onMounted(() => {
  void nextTick(() => primaryAction.value?.focus())
})
</script>

<style scoped>
.project-recovery {
  display: flex;
  max-width: 720px;
  gap: var(--space-5);
  margin: var(--space-6) auto;
  padding: var(--space-8);
}

.project-recovery__icon {
  display: grid;
  flex-shrink: 0;
  place-items: center;
  width: 56px;
  height: 56px;
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-warning-container);
  color: var(--md-sys-color-warning);
}

.project-recovery__body {
  display: grid;
  min-width: 0;
  gap: var(--space-3);
}

.project-recovery h1 {
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-headline-small);
}

.project-recovery__reason {
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-body-large);
}

.project-recovery__next,
.project-recovery__note {
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-medium);
}

.project-recovery__actions {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-2);
  margin-top: var(--space-2);
}

.project-recovery__link {
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-small);
}

.project-recovery__link summary {
  cursor: pointer;
}

.project-recovery__link code {
  overflow-wrap: anywhere;
}

.filled-button span {
  overflow: hidden;
  max-width: 360px;
  text-overflow: ellipsis;
  white-space: nowrap;
}

@media (max-width: 599px) {
  .project-recovery {
    flex-direction: column;
    padding: var(--space-5);
  }
}
</style>
