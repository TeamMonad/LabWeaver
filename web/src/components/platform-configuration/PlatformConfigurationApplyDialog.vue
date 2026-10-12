<template>
  <AlertDialogRoot :open="open" @update:open="(next) => !next && $emit('cancel')">
    <AlertDialogPortal>
      <AlertDialogOverlay class="pc-scrim" />
      <AlertDialogContent class="pc-dialog">
        <AlertDialogTitle class="pc-dialog__title">提交应用请求：desired revision {{ desiredRevision }}</AlertDialogTitle>
        <AlertDialogDescription class="pc-dialog__description">
          这只会记录一次应用请求（202），不会在浏览器中执行部署。获准执行者会使用既有部署入口执行，并报告校验、应用、验证阶段；只有完整验证成功后 applied 才会更新。
        </AlertDialogDescription>

        <div class="pc-dialog__impact">
          <strong>与最后验证 applied 相比：</strong>
          <ul v-if="impactItems.length">
            <li v-for="item in impactItems" :key="item">{{ item }}</li>
          </ul>
          <p v-else>三组设置均无差异。</p>
        </div>

        <p v-if="appliedState === 'unverified'" class="pc-dialog__warn">
          当前外部结果待核对。新尝试不会清除待核对状态；请先确认外部真实状态。
        </p>

        <label class="pc-dialog__field">
          <span>加密 Bundle 密文文件 SHA-256</span>
          <input
            v-model.trim="bundleSha"
            class="text-input"
            spellcheck="false"
            autocomplete="off"
            placeholder="64 位小写十六进制"
            :aria-invalid="bundleSha.length > 0 && !shaValid"
            data-testid="pc-bundle-sha"
          />
          <small>用获准方式准备与此 desired 对应的加密 Bundle，并对密文文件计算 SHA-256。浏览器不接触 Bundle 内容或密钥。</small>
          <small v-if="bundleSha.length > 0 && !shaValid" class="invalid">须为 64 位小写十六进制。</small>
        </label>

        <div class="pc-dialog__actions">
          <AlertDialogCancel class="text-button">取消</AlertDialogCancel>
          <button type="button" class="filled-button" :disabled="!shaValid || busy" data-testid="pc-apply-confirm" @click="$emit('confirm', bundleSha)">
            {{ busy ? '提交中…' : '记录应用请求' }}
          </button>
        </div>
      </AlertDialogContent>
    </AlertDialogPortal>
  </AlertDialogRoot>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import {
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogOverlay,
  AlertDialogPortal,
  AlertDialogRoot,
  AlertDialogTitle,
} from 'reka-ui'
import type { PlatformConfigurationAppliedState, PlatformConfigurationImpact } from '@/generated/contracts'
import { SHA256_PATTERN } from '@/utils/platformConfiguration'

const props = defineProps<{
  open: boolean
  desiredRevision: number
  impact: PlatformConfigurationImpact
  appliedState: PlatformConfigurationAppliedState
  busy: boolean
}>()

defineEmits<{ confirm: [sha256: string]; cancel: [] }>()

const bundleSha = ref('')
const shaValid = computed(() => SHA256_PATTERN.test(bundleSha.value))

watch(() => props.open, (open) => { if (open) bundleSha.value = '' })

const impactItems = computed(() => {
  const items: string[] = []
  if (props.impact.runtimePolicyChanged) items.push('运行策略有变化')
  if (props.impact.builderPolicyChanged) items.push('构建策略有变化')
  if (props.impact.lifecyclePolicyChanged) items.push('生命周期设置有变化')
  return items
})
</script>

<style scoped>
.pc-scrim { position: fixed; inset: 0; z-index: 1400; background: var(--md-sys-color-scrim); opacity: .32; }
.pc-dialog { position: fixed; top: 50%; left: 50%; z-index: 1450; display: grid; gap: 14px; width: min(560px, calc(100vw - 32px)); max-height: calc(100vh - 32px); overflow: auto; padding: 24px; border-radius: var(--md-sys-shape-extra-large, 28px); background: var(--md-sys-color-surface-container-high, var(--md-sys-color-surface)); box-shadow: var(--md-sys-elevation-3); transform: translate(-50%, -50%); outline: none; }
.pc-dialog__title { margin: 0; font: var(--md-sys-headline-small); color: var(--md-sys-color-on-surface); }
.pc-dialog__description, .pc-dialog__impact, .pc-dialog__warn { margin: 0; font: var(--md-sys-body-medium); color: var(--md-sys-color-on-surface-variant); line-height: 1.5; }
.pc-dialog__impact ul { margin: 4px 0 0; padding-left: 20px; }
.pc-dialog__impact p { margin: 4px 0 0; }
.pc-dialog__warn { padding: 8px 12px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-error-container); color: var(--md-sys-color-on-error-container); }
.pc-dialog__field { display: grid; gap: 6px; font: var(--md-sys-label-medium); color: var(--md-sys-color-on-surface-variant); }
.pc-dialog__field small { font: var(--md-sys-body-small); }
.invalid { color: var(--md-sys-color-error); }
.text-input { box-sizing: border-box; min-height: 40px; width: 100%; padding: 8px 11px; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface); color: var(--md-sys-color-on-surface); font: 13px/1.4 var(--md-sys-typescale-mono, monospace); }
.pc-dialog__actions { display: flex; justify-content: flex-end; gap: 8px; }
.text-button { min-height: 40px; padding: 0 12px; border: 0; background: transparent; color: var(--md-sys-color-primary); font: var(--md-sys-label-large); cursor: pointer; }
.filled-button { min-height: 40px; padding: 0 20px; border: 0; border-radius: var(--md-sys-shape-full); background: var(--md-sys-color-primary); color: var(--md-sys-color-on-primary); font: var(--md-sys-label-large); cursor: pointer; }
.filled-button:disabled { opacity: .5; cursor: not-allowed; }
</style>
