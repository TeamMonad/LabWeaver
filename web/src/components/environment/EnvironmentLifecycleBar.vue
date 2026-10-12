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
          class="text-button error"
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
  margin-top: 16px;
}

.lifecycle-action-hint {
  margin: 8px 0 0;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-small);
}

.filled-button {
  height: 40px;
  padding: 0 24px;
  border: none;
  border-radius: var(--md-sys-shape-full);
  background: var(--md-sys-color-primary);
  color: var(--md-sys-color-on-primary);
  font: var(--md-sys-label-large);
  cursor: pointer;
}

.filled-button.small {
  height: 32px;
  padding: 0 16px;
}

.filled-button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.outlined-button {
  height: 40px;
  padding: 0 24px;
  border: 1px solid var(--md-sys-color-outline);
  border-radius: var(--md-sys-shape-full);
  background: transparent;
  color: var(--md-sys-color-primary);
  font: var(--md-sys-label-large);
  cursor: pointer;
}

.outlined-button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.text-button {
  height: 40px;
  padding: 0 16px;
  border: none;
  border-radius: var(--md-sys-shape-full);
  background: transparent;
  color: var(--md-sys-color-primary);
  font: var(--md-sys-label-large);
  cursor: pointer;
}

.text-button.error {
  color: var(--md-sys-color-error);
}

.text-button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
</style>
