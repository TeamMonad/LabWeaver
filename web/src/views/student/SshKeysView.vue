<template>
  <div class="ssh-keys">
    <header class="page-header">
      <h2>SSH 公钥</h2>
      <p class="page-subtitle">登记公钥后可通过 SSH 进入容器或 VM 环境。平台不存储、不生成私钥。</p>
    </header>

    <section class="add-section" aria-labelledby="add-heading">
      <h3 id="add-heading" class="section-title">添加公钥</h3>
      <div class="key-form">
        <textarea
          v-model="newKey"
          rows="4"
          class="key-input"
          placeholder="粘贴 OpenSSH 公钥，例如 ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI..."
          aria-label="OpenSSH 公钥"
        />
        <div class="form-actions">
          <button
            type="button"
            class="filled-button"
            :disabled="!newKey.trim() || sshKeys.creating"
            @click="submitKey"
          >
            添加
          </button>
        </div>
      </div>
      <DiagnosticBanner
        v-if="addError"
        class="form-error"
        :code="addError.code"
        :message="addError.message"
        :retryable="addError.retryable"
        severity="error"
        @retry="submitKey"
      />
    </section>

    <section aria-labelledby="list-heading">
      <h3 id="list-heading" class="section-title">已登记公钥</h3>
      <AsyncStateView :state="sshKeys.keys" @retry="sshKeys.load">
        <template #success="{ data }">
          <DataTable :columns="keyColumns" :rows="data" aria-label="已登记 SSH 公钥">
            <template #fingerprintSha256="{ row }">
              <code :title="row.fingerprintSha256">{{ truncateSha256(row.fingerprintSha256) }}</code>
            </template>
            <template #algorithm="{ row }">
              <span class="tag">{{ row.algorithm }}</span>
            </template>
            <template #createdAt="{ row }">
              {{ formatTimestamp(row.createdAt) }}
            </template>
            <template #actions="{ row }">
              <button
                type="button"
                class="icon-button text-button"
                :disabled="sshKeys.deleting.has(row.id)"
                aria-label="删除"
                @click="openDelete(row)"
              >
                <SvgIcon name="delete" size="sm" aria-hidden="true" />
              </button>
            </template>
          </DataTable>
        </template>
      </AsyncStateView>
    </section>

    <ConfirmDialog
      :open="deleteTarget !== null"
      title="删除 SSH 公钥"
      :description="`确定删除指纹为 ${deleteTarget ? truncateSha256(deleteTarget.fingerprintSha256) : ''} 的公钥吗？此操作不可恢复。`"
      confirm-text="删除"
      severity="error"
      @cancel="deleteTarget = null"
      @confirm="confirmDelete"
    />
  </div>
</template>

<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { useSshPublicKeys } from '@/composables/useSshPublicKeys'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import DataTable from '@/components/common/DataTable.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import ConfirmDialog from '@/components/common/ConfirmDialog.vue'
import SvgIcon from '@/components/common/SvgIcon.vue'
import { truncateSha256, formatTimestamp } from '@/utils/format'
import type { DataTableColumn } from '@/components/common/DataTable.vue'
import type { SshPublicKeySchema } from '@/generated/contracts'
import type { DiagnosticViewModel } from '@/types/async'

const sshKeys = useSshPublicKeys()
onMounted(() => sshKeys.load())
const newKey = ref('')
const addError = ref<DiagnosticViewModel | null>(null)
const deleteTarget = ref<SshPublicKeySchema | null>(null)

const keyColumns: DataTableColumn<SshPublicKeySchema & { actions?: never }>[] = [
  { key: 'algorithm', title: '算法' },
  { key: 'fingerprintSha256', title: '指纹' },
  { key: 'createdAt', title: '添加时间' },
  { key: 'actions', title: '操作' },
]

async function submitKey() {
  addError.value = null
  const result = await sshKeys.add(newKey.value.trim())
  if (result.ok) {
    newKey.value = ''
  } else {
    addError.value = result.diagnostic
  }
}

function openDelete(key: SshPublicKeySchema) {
  deleteTarget.value = key
}

async function confirmDelete() {
  if (!deleteTarget.value) return
  const result = await sshKeys.remove(deleteTarget.value)
  deleteTarget.value = null
  if (!result.ok) {
    addError.value = result.diagnostic
  }
}
</script>

<style scoped>
.ssh-keys {
  display: flex;
  flex-direction: column;
  gap: 28px;
}

.page-header h2 {
  font: var(--md-sys-headline-small);
  color: var(--md-sys-color-on-surface);
  margin: 0;
}

.page-subtitle {
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface-variant);
  margin: 4px 0 0;
}

.section-title {
  font: var(--md-sys-title-medium);
  color: var(--md-sys-color-on-surface);
  margin: 0 0 12px;
}

.key-form {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.key-input {
  width: 100%;
  min-height: 96px;
  padding: 12px;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-surface-container-low);
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-body-medium);
  resize: vertical;
}

.key-input:focus-visible {
  outline: 2px solid var(--md-sys-color-primary);
  outline-offset: 2px;
}

.form-actions {
  display: flex;
  justify-content: flex-end;
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

.filled-button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.text-button {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 32px;
  height: 32px;
  padding: 0;
  border: none;
  border-radius: var(--md-sys-shape-full);
  background: transparent;
  color: var(--md-sys-color-on-surface-variant);
  cursor: pointer;
}

.text-button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.form-error {
  margin-top: 12px;
}

.tag {
  padding: 4px 10px;
  border-radius: var(--md-sys-shape-small);
  background: var(--md-sys-color-secondary-container);
  color: var(--md-sys-color-on-secondary-container);
  font: var(--md-sys-label-medium);
}
</style>
