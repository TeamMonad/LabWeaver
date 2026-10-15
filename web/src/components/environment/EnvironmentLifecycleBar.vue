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
          :disabled="retryingEnvironment"
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
import { deleteDialogCopy } from '@/utils/environmentLifecycle'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import ConfirmDialog from '@/components/common/ConfirmDialog.vue'
import GcpActionBar from '@/components/common/GcpActionBar.vue'
import { useEnvironmentEntryContext } from '@/composables/useEnvironmentEntry'
import { failedCleanup, lifecycleConfirmationText } from '@/utils/environmentLifecycle'

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
  lifecycleActionReason,
  lifecycleActionHint,
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
