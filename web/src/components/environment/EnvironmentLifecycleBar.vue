<template>
  <div class="lifecycle-bar">
    <GcpActionBar
      :refreshing="env.instance.kind === 'loading'"
      @refresh="refreshAll"
    >
      <template v-if="env.instance.kind === 'success'">
        <button
          type="button"
          class="filled-button"
          :disabled="!canStart(env.instance.data)"
          :title="lifecycleActionReason(env.instance.data, 'start')"
          aria-label="启动"
          aria-describedby="lifecycle-action-hint"
          @click="runLifecycle(env.instance.data, 'start')"
        >
          启动
        </button>
        <button
          type="button"
          class="outlined-button"
          :disabled="!canStop(env.instance.data)"
          :title="lifecycleActionReason(env.instance.data, 'stop')"
          aria-label="停止"
          aria-describedby="lifecycle-action-hint"
          @click="requestLifecycleConfirmation(env.instance.data, 'stop')"
        >
          停止
        </button>
        <button
          type="button"
          class="outlined-button"
          :disabled="!canRestart(env.instance.data)"
          :title="lifecycleActionReason(env.instance.data, 'restart')"
          aria-label="重启"
          aria-describedby="lifecycle-action-hint"
          @click="requestLifecycleConfirmation(env.instance.data, 'restart')"
        >
          重启
        </button>
        <button
          v-if="retryableOperation"
          type="button"
          class="outlined-button"
          :disabled="retryingEnvironment || !canRetryFailedOperation()"
          :title="permissions.denialReason('edit', '重试失败的操作') ?? undefined"
          @click="retryFailedOperation(env.instance.data)"
        >
          重试失败的操作
        </button>
        <button
          type="button"
          class="outlined-button danger-button"
          :disabled="!canDelete(env.instance.data)"
          :title="lifecycleActionReason(env.instance.data, 'delete')"
          :aria-label="failedCleanup(env.instance.data) ? '重试回收' : '删除'"
          aria-describedby="lifecycle-action-hint"
          @click="openDelete(env.instance.data)"
        >
          {{ failedCleanup(env.instance.data) ? '重试回收' : '删除' }}
        </button>
      </template>
    </GcpActionBar>

    <p
      v-if="env.instance.kind === 'success'"
      id="lifecycle-action-hint"
      class="lifecycle-action-hint"
      role="status"
    >
      {{ lifecycleActionHint(env.instance.data) }}
    </p>
    <p
      v-if="env.instance.kind === 'success' && lifecyclePermissionHint()"
      class="lifecycle-action-hint lifecycle-permission-hint"
      data-testid="lifecycle-permission-hint"
      role="note"
    >
      {{ lifecyclePermissionHint() }}
    </p>

    <div
      v-if="lifecycleDiagnostic"
      class="lifecycle-result"
    >
      <DiagnosticBanner
        :code="lifecycleDiagnostic.code"
        :request-id="lifecycleDiagnostic.requestId"
        :next-step="lifecycleDiagnostic.nextStep"
        :message="lifecycleDiagnostic.message"
        :retryable="lifecycleDiagnostic.retryable && !(env.instance.kind === 'success' && env.instance.data.desiredState === 'deleted')"
        severity="error"
        @retry="retryLifecycle"
      />
      <RouterLink
        v-if="requiresNewResourceRequest(lifecycleDiagnostic.code) && env.instance.kind === 'success' && env.instance.data.class === 'work'"
        class="outlined-button small lease-request-link"
        data-testid="lease-new-request-link"
        :to="{ path: '/researcher/resources', query: { projectId: env.instance.data.projectId, releaseId: env.instance.data.releaseId, releaseVersion: String(env.instance.data.releaseVersion) } }"
      >
        提交新的资源申请
      </RouterLink>
    </div>

    <ConfirmDialog
      :open="deleteEnvironment !== null"
      :title="deleteCopy.title"
      :description="deleteCopy.description"
      :confirm-text="deleteCopy.confirmText"
      severity="error"
      @cancel="deleteEnvironment = null"
      @confirm="confirmDeleteEnvironment"
    />

    <ConfirmDialog
      :open="lifecycleConfirmation !== null"
      :title="lifecycleConfirmation?.action === 'restart' ? '重启环境' : '停止环境'"
      :description="lifecycleConfirmation ? lifecycleConfirmationText(lifecycleConfirmation.data, lifecycleConfirmation.action) : ''"
      :confirm-text="lifecycleConfirmation?.action === 'restart' ? '重启' : '停止'"
      severity="warning"
      @cancel="lifecycleConfirmation = null"
      @confirm="confirmLifecycle"
    />
  </div>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { RouterLink } from 'vue-router'
import { deleteDialogCopy } from '@/utils/environmentLifecycle'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import ConfirmDialog from '@/components/common/ConfirmDialog.vue'
import GcpActionBar from '@/components/common/GcpActionBar.vue'
import { useEnvironmentEntryContext } from '@/composables/useEnvironmentEntry'
import { failedCleanup, lifecycleConfirmationText } from '@/utils/environmentLifecycle'
import { requiresNewResourceRequest } from '@/types/async'

/** 生命周期操作栏：启动、停止、重启、重试和删除，以及停止/重启/删除确认。 */
const {
  env,
  retryableOperation,
  refreshAll,
  lifecycleDiagnostic,
  retryingEnvironment,
  deleteEnvironment,
  lifecycleConfirmation,
  canStart,
  canStop,
  canRestart,
  canDelete,
  canRetryFailedOperation,
  permissions,
  lifecycleActionReason,
  lifecycleActionHint,
  lifecyclePermissionHint,
  runLifecycle,
  requestLifecycleConfirmation,
  confirmLifecycle,
  retryFailedOperation,
  retryLifecycle,
  openDelete,
  confirmDeleteEnvironment,
} = useEnvironmentEntryContext()

const deleteCopy = computed(() => deleteDialogCopy(deleteEnvironment.value))
</script>

<style scoped>
/* Layout wrapper only: its children stay in the parent's flex layout. */
.lifecycle-bar {
  display: contents;
}

.lifecycle-result {
  margin-top: var(--space-4);
}

.lease-request-link {
  display: inline-flex;
  margin-top: var(--space-2);
}

.lifecycle-action-hint {
  margin: var(--space-2) 0 0;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-small);
}

/* Destructive action sits apart from lifecycle actions. */
.danger-button {
  position: relative;
  margin-inline-start: var(--space-4);
}

.danger-button::before {
  content: '';
  position: absolute;
  inset-inline-start: calc(-1 * var(--space-3));
  top: 8px;
  bottom: 8px;
  width: 1px;
  background: var(--md-sys-color-outline-variant);
  pointer-events: none;
}
</style>
