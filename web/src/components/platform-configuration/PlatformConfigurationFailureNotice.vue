<template>
  <div class="failure-notice" role="alert" data-testid="platform-configuration-failure">
    <SvgIcon name="error" size="md" aria-hidden="true" />
    <div class="failure-notice__body">
      <p class="failure-notice__message">{{ failure.message }}</p>
      <p v-if="failure.recovery" class="failure-notice__recovery">{{ failure.recovery }}</p>
      <div class="failure-notice__ids">
        <code>{{ failure.code }}</code>
        <code v-if="failure.requestId">requestId {{ failure.requestId }}</code>
        <button type="button" class="text-button" @click="copyIdentifiers">{{ copied ? '已复制' : '复制诊断信息' }}</button>
      </div>
    </div>
    <button v-if="dismissible" type="button" class="icon-button" aria-label="关闭提示" @click="$emit('dismiss')">
      <SvgIcon name="close" size="sm" aria-hidden="true" />
    </button>
  </div>
</template>

<script setup lang="ts">
import { ref } from 'vue'
import SvgIcon from '@/components/common/SvgIcon.vue'
import type { PlatformConfigurationFailure } from '@/composables/usePlatformConfiguration'

const props = withDefaults(defineProps<{ failure: PlatformConfigurationFailure; dismissible?: boolean }>(), { dismissible: true })
defineEmits<{ dismiss: [] }>()

const copied = ref(false)

async function copyIdentifiers() {
  const lines = [`code: ${props.failure.code}`]
  if (props.failure.requestId) lines.push(`requestId: ${props.failure.requestId}`)
  if (props.failure.traceId) lines.push(`traceId: ${props.failure.traceId}`)
  try {
    await navigator.clipboard?.writeText(lines.join('\n'))
    copied.value = true
  } catch {
    copied.value = false
  }
}
</script>

<style scoped>
.failure-notice { display: flex; align-items: flex-start; gap: 12px; padding: 12px 16px; border-radius: var(--md-sys-shape-medium); background: var(--md-sys-color-error-container); color: var(--md-sys-color-on-error-container); }
.failure-notice__body { display: grid; flex: 1; gap: 4px; min-width: 0; }
.failure-notice__message { margin: 0; font: var(--md-sys-title-small); }
.failure-notice__recovery { margin: 0; font: var(--md-sys-body-medium); }
.failure-notice__ids { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; font: var(--md-sys-body-small); }
.failure-notice__ids code { overflow-wrap: anywhere; opacity: .85; }
.text-button { border: 0; background: transparent; color: inherit; font: var(--md-sys-label-medium); text-decoration: underline; cursor: pointer; padding: 0; }
.icon-button { display: inline-grid; place-items: center; width: 32px; height: 32px; border: 0; border-radius: 50%; background: transparent; color: inherit; cursor: pointer; }
</style>
