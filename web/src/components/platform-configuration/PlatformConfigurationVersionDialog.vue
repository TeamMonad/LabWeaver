<template>
  <DialogRoot :open="revision !== null" @update:open="(next) => !next && $emit('close')">
    <DialogPortal>
      <DialogOverlay class="pc-scrim" />
      <DialogContent class="pc-version" :aria-describedby="undefined">
        <header class="pc-version__header">
          <DialogTitle class="pc-version__title">配置 revision {{ revision }}</DialogTitle>
          <DialogClose class="icon-button" aria-label="关闭版本详情">
            <SvgIcon name="close" size="sm" aria-hidden="true" />
          </DialogClose>
        </header>
        <p v-if="loading" class="muted">读取中…</p>
        <PlatformConfigurationFailureNotice v-else-if="failure" :failure="failure" :dismissible="false" />
        <template v-else-if="version">
          <p class="muted">{{ formatTimestamp(version.createdAt) }} · {{ version.createdBy }} · <code>{{ version.configurationSha256 }}</code></p>
          <pre class="pc-version__yaml" data-testid="pc-version-yaml">{{ yamlText }}</pre>
        </template>
      </DialogContent>
    </DialogPortal>
  </DialogRoot>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { stringify } from 'yaml'
import { DialogClose, DialogContent, DialogOverlay, DialogPortal, DialogRoot, DialogTitle } from 'reka-ui'
import SvgIcon from '@/components/common/SvgIcon.vue'
import type { PlatformConfigurationVersionSchema } from '@/generated/contracts'
import type { PlatformConfigurationFailure } from '@/composables/usePlatformConfiguration'
import { formatTimestamp } from '@/utils/format'
import PlatformConfigurationFailureNotice from './PlatformConfigurationFailureNotice.vue'

const props = defineProps<{
  revision: number | null
  version: PlatformConfigurationVersionSchema | null
  loading: boolean
  failure: PlatformConfigurationFailure | null
}>()

defineEmits<{ close: [] }>()

const yamlText = computed(() => (props.version ? stringify(props.version.configuration, { lineWidth: 0 }) : ''))
</script>

<style scoped>
.pc-scrim { position: fixed; inset: 0; z-index: 1400; background: var(--md-sys-color-scrim); opacity: .32; }
.pc-version { position: fixed; top: 50%; left: 50%; z-index: 1450; display: grid; gap: 12px; width: min(640px, calc(100vw - 32px)); max-height: calc(100vh - 48px); overflow: auto; padding: 20px 24px; border-radius: var(--md-sys-shape-extra-large, 28px); background: var(--md-sys-color-surface-container-high, var(--md-sys-color-surface)); box-shadow: var(--md-sys-elevation-3); transform: translate(-50%, -50%); outline: none; }
.pc-version__header { display: flex; justify-content: space-between; align-items: center; gap: 12px; }
.pc-version__title { margin: 0; font: var(--md-sys-title-large); color: var(--md-sys-color-on-surface); }
.pc-version__yaml { margin: 0; padding: 12px; overflow: auto; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-variant); font: 12px/1.5 monospace; white-space: pre; }
.muted { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); overflow-wrap: anywhere; }
.icon-button { display: inline-grid; place-items: center; width: 40px; height: 40px; border: 0; border-radius: 50%; background: transparent; color: var(--md-sys-color-on-surface-variant); cursor: pointer; }
</style>
