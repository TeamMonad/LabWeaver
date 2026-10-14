<template>
  <div v-if="message || cancellable" class="upload-recovery" data-testid="upload-recovery">
    <p v-if="message" role="status">{{ message }}</p>
    <ul v-if="state.kind === 'needs-files'" class="upload-recovery__missing" aria-label="尚未上传的文件">
      <li v-for="path in state.missing" :key="path"><code>{{ path }}</code></li>
    </ul>
    <button
      v-if="cancellable"
      type="button"
      class="text-button"
      :disabled="state.kind === 'cancelling'"
      @click="emit('cancel')"
    >
      {{ state.kind === 'cancelling' ? '正在取消…' : '取消上传' }}
    </button>
  </div>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import type { PackageUploadState } from '@/composables/useProjectProblemPackageUpload'

const props = defineProps<{ state: PackageUploadState; hasSession: boolean }>()
const emit = defineEmits<{ cancel: [] }>()

const message = computed(() => {
  switch (props.state.kind) {
    case 'needs-files':
      return '找到上次未完成的上传。请重新选择相同的文件，只会补传下面尚未上传的文件。'
    case 'cancelling':
      return '正在取消上传，已停止发送文件。'
    case 'cancelled':
      return '上传已取消，未发布材料包。已上传的临时文件会在授权到期后由平台清理。'
    default:
      return ''
  }
})

const cancellable = computed(() => props.hasSession
  && ['uploading', 'completing', 'needs-files', 'cancelling'].includes(props.state.kind))
</script>

<style scoped>
.upload-recovery { display: grid; gap: 6px; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.upload-recovery p { margin: 0; }
.upload-recovery__missing { margin: 0; padding-left: 18px; }
.upload-recovery .text-button { justify-self: start; }
</style>
