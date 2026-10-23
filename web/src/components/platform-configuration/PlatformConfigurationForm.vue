<template>
  <div class="pc-form">
    <fieldset class="group">
      <legend>生命周期</legend>
      <p class="hint">单位为秒。留存与提醒须在 1 秒到 365 天之间；提醒最多 16 个且不能重复；抢占宽限 1–300 秒。这些值目前只保存为配置，清理、提醒和抢占消费者尚未接入。</p>
      <div class="grid">
        <label>
          <span>只读保留</span>
          <input v-model.number="model.lifecycle.readOnlyRetentionSeconds" class="text-input" type="number" min="1" max="31536000" step="1" data-testid="pc-readonly-retention" />
          <small>{{ formatSeconds(model.lifecycle.readOnlyRetentionSeconds) }}</small>
        </label>
        <label>
          <span>详细日志保留</span>
          <input v-model.number="model.lifecycle.detailedLogRetentionSeconds" class="text-input" type="number" min="1" max="31536000" step="1" />
          <small>{{ formatSeconds(model.lifecycle.detailedLogRetentionSeconds) }}</small>
        </label>
        <label>
          <span>到期提醒（逗号分隔）</span>
          <input v-model="reminderText" class="text-input" inputmode="numeric" :aria-invalid="reminderInvalid" data-testid="pc-reminders" @change="commitReminders" />
          <small :class="{ invalid: reminderInvalid }">{{ reminderInvalid ? '请输入正整数秒，用逗号分隔。' : model.lifecycle.expiryReminderSeconds.map(formatSeconds).join(' / ') }}</small>
        </label>
        <label>
          <span>抢占宽限</span>
          <input v-model.number="model.lifecycle.preemptionGraceSeconds" class="text-input" type="number" min="1" max="300" step="1" data-testid="pc-preemption-grace" />
          <small>{{ formatSeconds(model.lifecycle.preemptionGraceSeconds) }}</small>
        </label>
      </div>
    </fieldset>

    <fieldset class="group">
      <legend>运行策略</legend>
      <div class="grid">
        <label>
          <span>特权运行</span>
          <AppSelect v-model="privilegedMode" data-testid="pc-privileged-mode">
            <option value="none">不允许特权</option>
            <option value="dedicated_node_pool">允许，仅在专用节点池运行</option>
            <option value="virtual_machine_guest" disabled>允许，在 VM guest 运行（尚无执行侧实现）</option>
          </AppSelect>
          <small>特权必须绑定专用节点池（nodeSelector + NoSchedule taint）；仅声明 namespace 不构成执行位置，也不会放宽 namespace 准入。</small>
        </label>
      </div>
      <div v-if="location?.kind === 'dedicated_node_pool'" class="grid" data-testid="pc-node-pool">
        <label>
          <span>nodeSelector（每行 key=value）</span>
          <textarea v-model="selectorText" class="text-input" rows="3" :aria-invalid="selectorInvalid" @change="commitSelector" />
          <small v-if="selectorInvalid" class="invalid">每行须为 key=value。</small>
        </label>
        <label>
          <span>taint key</span>
          <input v-model="location.taintKey" class="text-input" />
        </label>
        <label>
          <span>taint value</span>
          <input v-model="location.taintValue" class="text-input" />
        </label>
      </div>

      <dl class="fixed-list" data-testid="pc-runtime-fixed">
        <div>
          <dt>默认容器 UID</dt>
          <dd><code>{{ model.runtimePolicy.defaultContainerUid }}</code></dd>
        </div>
        <div>
          <dt>普通容器 capabilities 允许清单</dt>
          <dd>{{ listText(model.runtimePolicy.allowedCapabilities) }}</dd>
        </div>
        <div>
          <dt>项目 / 课程预授权</dt>
          <dd>{{ model.runtimePolicy.preauthorizations.length ? `${model.runtimePolicy.preauthorizations.length} 条` : '无' }}</dd>
        </div>
      </dl>
      <p class="hint">以上三项尚无执行侧实现（见 #207）：只接受 UID 0、空允许清单和空预授权，非默认值会被 Bundle 导出与部署渲染拒绝，因此这里不可编辑。</p>
    </fieldset>

    <fieldset class="group">
      <legend>构建策略</legend>
      <dl class="fixed-list" data-testid="pc-builder-fixed">
        <div>
          <dt>builder capabilities</dt>
          <dd>{{ listText(model.builderPolicy.capabilities) }}</dd>
        </div>
        <div>
          <dt>允许提权</dt>
          <dd>{{ model.builderPolicy.allowPrivilegeEscalation ? '是' : '否' }}</dd>
        </div>
        <div>
          <dt>entitlements</dt>
          <dd>{{ listText(model.builderPolicy.entitlements) }}</dd>
        </div>
      </dl>
      <p class="hint">builder 使用独立的固定安全上下文；修改运行特权不会扩大 builder 权限。builder capabilities、提权和 entitlements 尚无执行侧实现（见 #207），只接受空值与关闭。</p>
    </fieldset>
  </div>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import AppSelect from '@/components/form/AppSelect.vue'
import type { PlatformConfigurationDesired } from '@/generated/contracts'
import { formatSeconds, formatSelector, parseSecondsList, parseSelector } from '@/utils/platformConfiguration'

const model = defineModel<PlatformConfigurationDesired>({ required: true })

const reminderText = ref('')
const reminderInvalid = ref(false)
const selectorText = ref('')
const selectorInvalid = ref(false)

watch(
  () => model.value.lifecycle.expiryReminderSeconds,
  (values) => {
    reminderText.value = values.join(', ')
    reminderInvalid.value = false
  },
  { immediate: true, deep: true },
)

const location = computed(() => model.value.runtimePolicy.privilegedExecutionLocation ?? null)

watch(
  () => (location.value?.kind === 'dedicated_node_pool' ? location.value.nodeSelector : null),
  (selector) => {
    selectorText.value = selector ? formatSelector(selector) : ''
    selectorInvalid.value = false
  },
  { immediate: true, deep: true },
)

function commitReminders() {
  const parsed = parseSecondsList(reminderText.value)
  reminderInvalid.value = parsed === null
  if (parsed) model.value.lifecycle.expiryReminderSeconds = parsed
}

function commitSelector() {
  const parsed = parseSelector(selectorText.value)
  selectorInvalid.value = parsed === null
  if (parsed && location.value?.kind === 'dedicated_node_pool') location.value.nodeSelector = parsed
}

/** `privilegedAllowed` and the location move together, so the form cannot express the invalid half-states. */
const privilegedMode = computed<string | number | null | undefined>({
  get: () => location.value?.kind ?? 'none',
  set: (mode) => {
    const policy = model.value.runtimePolicy
    if (mode === (policy.privilegedExecutionLocation?.kind ?? 'none')) return
    if (mode === 'dedicated_node_pool') {
      policy.privilegedAllowed = true
      policy.privilegedExecutionLocation = { kind: 'dedicated_node_pool', nodeSelector: {}, taintKey: '', taintValue: '' }
    } else {
      policy.privilegedAllowed = false
      policy.privilegedExecutionLocation = null
    }
  },
})

function listText(values: readonly string[]): string {
  return values.length ? values.join('、') : '无'
}
</script>

<style scoped>
.pc-form { display: grid; gap: var(--space-4); }
.group { display: grid; gap: var(--space-3); margin: 0; padding: var(--space-4); border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-medium); }
legend { padding: 0 var(--space-2); font: var(--md-sys-title-medium); color: var(--md-sys-color-on-surface); }
.hint { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); line-height: 1.5; }
.grid { display: grid; gap: var(--space-3); grid-template-columns: repeat(auto-fit, minmax(200px, 1fr)); align-items: start; }
.grid label { display: grid; gap: var(--space-2); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.grid small { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); line-height: 1.4; }
.invalid { color: var(--md-sys-color-error) !important; }
.fixed-list { display: grid; grid-template-columns: repeat(auto-fit, minmax(180px, 1fr)); gap: var(--space-2) var(--space-4); margin: 0; padding: var(--space-3); border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low); }
.fixed-list dt { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.fixed-list dd { margin: var(--space-1) 0 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); overflow-wrap: anywhere; }
</style>
