<template>
  <div class="pc-form">
    <fieldset class="group">
      <legend>生命周期</legend>
      <p class="hint">单位为秒。留存与提醒须在 1 秒到 365 天之间；抢占宽限 1–300 秒。</p>
      <div class="grid">
        <label>
          <span>只读保留</span>
          <input v-model.number="model.lifecycle.readOnlyRetentionSeconds" class="text-input" type="number" min="1" step="1" data-testid="pc-readonly-retention" />
          <small>{{ formatSeconds(model.lifecycle.readOnlyRetentionSeconds) }}</small>
        </label>
        <label>
          <span>详细日志保留</span>
          <input v-model.number="model.lifecycle.detailedLogRetentionSeconds" class="text-input" type="number" min="1" step="1" />
          <small>{{ formatSeconds(model.lifecycle.detailedLogRetentionSeconds) }}</small>
        </label>
        <label>
          <span>到期提醒（逗号分隔，最多 16 个）</span>
          <input v-model="reminderText" class="text-input" inputmode="numeric" :aria-invalid="reminderInvalid" @change="commitReminders" />
          <small :class="{ invalid: reminderInvalid }">{{ reminderInvalid ? '请输入正整数秒，用逗号分隔。' : model.lifecycle.expiryReminderSeconds.map(formatSeconds).join(' / ') }}</small>
        </label>
        <label>
          <span>抢占宽限</span>
          <input v-model.number="model.lifecycle.preemptionGraceSeconds" class="text-input" type="number" min="1" max="300" step="1" />
          <small>{{ formatSeconds(model.lifecycle.preemptionGraceSeconds) }}</small>
        </label>
      </div>
    </fieldset>

    <fieldset class="group">
      <legend>运行策略</legend>
      <div class="grid">
        <label>
          <span>默认容器 UID</span>
          <input v-model.number="model.runtimePolicy.defaultContainerUid" class="text-input" type="number" min="0" step="1" />
        </label>
        <div class="switch-row">
          <SwitchRoot id="pc-privileged" v-model="model.runtimePolicy.privilegedAllowed" class="switch">
            <SwitchThumb class="switch__thumb" />
          </SwitchRoot>
          <label for="pc-privileged">允许特权准入</label>
        </div>
      </div>

      <CapabilityPicker v-model="model.runtimePolicy.allowedCapabilities" label="平台允许的 capabilities" />

      <div class="subgroup">
        <span class="subgroup__title">特权执行位置</span>
        <p class="hint">特权准入必须声明专用节点池（selector + taint）或 VM guest provider binding；仅声明 namespace 不构成执行位置。</p>
        <RadioGroupRoot v-model="locationKind" class="radio-row" aria-label="特权执行位置">
          <label v-for="option in locationOptions" :key="option.value" class="radio">
            <RadioGroupItem :value="option.value" class="radio__item"><RadioGroupIndicator class="radio__dot" /></RadioGroupItem>
            <span>{{ option.label }}</span>
          </label>
        </RadioGroupRoot>
        <div v-if="location?.kind === 'dedicated_node_pool'" class="grid">
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
        <div v-else-if="location?.kind === 'virtual_machine_guest'" class="grid">
          <label>
            <span>provider binding</span>
            <input v-model="location.providerBinding" class="text-input" />
          </label>
        </div>
      </div>

      <div class="subgroup">
        <span class="subgroup__title">项目 / 课程预授权</span>
        <p class="hint">预授权不能超过平台允许清单，同一 scope 不能重复。</p>
        <p v-if="model.runtimePolicy.preauthorizations.length === 0" class="hint">暂无预授权。</p>
        <div v-for="(entry, index) in model.runtimePolicy.preauthorizations" :key="index" class="preauth">
          <div class="grid">
            <label>
              <span>范围</span>
              <select :value="entry.scope.kind" class="text-input" @change="setScopeKind(index, ($event.target as HTMLSelectElement).value)">
                <option value="project">项目</option>
                <option value="course">课程</option>
              </select>
            </label>
            <label>
              <span>{{ entry.scope.kind === 'project' ? '项目 ID' : '课程 ID' }}</span>
              <input v-if="entry.scope.kind === 'project'" v-model="entry.scope.projectId" class="text-input" />
              <input v-else v-model="entry.scope.courseId" class="text-input" />
            </label>
            <div class="switch-row">
              <SwitchRoot :id="`pc-preauth-privileged-${index}`" v-model="entry.privileged" class="switch">
                <SwitchThumb class="switch__thumb" />
              </SwitchRoot>
              <label :for="`pc-preauth-privileged-${index}`">特权</label>
            </div>
          </div>
          <CapabilityPicker v-model="entry.capabilities" :label="`预授权 capabilities #${index + 1}`" :options="model.runtimePolicy.allowedCapabilities" />
          <button type="button" class="text-button danger" @click="model.runtimePolicy.preauthorizations.splice(index, 1)">移除此预授权</button>
        </div>
        <button type="button" class="outlined-button" @click="addPreauthorization">添加预授权</button>
      </div>
    </fieldset>

    <fieldset class="group">
      <legend>构建策略</legend>
      <p class="hint">构建策略与运行策略独立：修改运行特权或 capabilities 不会扩大 builder 权限。</p>
      <CapabilityPicker v-model="model.builderPolicy.capabilities" label="builder capabilities" />
      <div class="switch-row">
        <SwitchRoot id="pc-builder-escalation" v-model="model.builderPolicy.allowPrivilegeEscalation" class="switch">
          <SwitchThumb class="switch__thumb" />
        </SwitchRoot>
        <label for="pc-builder-escalation">允许提权（allowPrivilegeEscalation）</label>
      </div>
      <div class="checks" role="group" aria-label="builder entitlements">
        <label v-for="entitlement in BUILDER_ENTITLEMENTS" :key="entitlement" class="check">
          <input v-model="model.builderPolicy.entitlements" type="checkbox" :value="entitlement" />
          <code>{{ entitlement }}</code>
        </label>
      </div>
    </fieldset>
  </div>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { RadioGroupIndicator, RadioGroupItem, RadioGroupRoot, SwitchRoot, SwitchThumb } from 'reka-ui'
import type { PlatformConfigurationDesired } from '@/generated/contracts'
import { BUILDER_ENTITLEMENTS, formatSeconds, formatSelector, parseSecondsList, parseSelector } from '@/utils/platformConfiguration'
import CapabilityPicker from './PlatformConfigurationCapabilityPicker.vue'

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

const locationOptions = [
  { value: 'none', label: '未声明' },
  { value: 'dedicated_node_pool', label: '专用节点池' },
  { value: 'virtual_machine_guest', label: 'VM guest' },
] as const

const locationKind = computed<string>({
  get: () => location.value?.kind ?? 'none',
  set: (kind) => {
    const policy = model.value.runtimePolicy
    if (kind === (policy.privilegedExecutionLocation?.kind ?? 'none')) return
    if (kind === 'dedicated_node_pool') policy.privilegedExecutionLocation = { kind, nodeSelector: {}, taintKey: '', taintValue: '' }
    else if (kind === 'virtual_machine_guest') policy.privilegedExecutionLocation = { kind, providerBinding: '' }
    else policy.privilegedExecutionLocation = null
  },
})

function setScopeKind(index: number, kind: string) {
  const entry = model.value.runtimePolicy.preauthorizations[index]
  if (!entry || entry.scope.kind === kind) return
  entry.scope = kind === 'course' ? { kind: 'course', courseId: '' } : { kind: 'project', projectId: '' }
}

function addPreauthorization() {
  model.value.runtimePolicy.preauthorizations.push({ scope: { kind: 'project', projectId: '' }, capabilities: [], privileged: false })
}
</script>

<style scoped>
.pc-form { display: grid; gap: 16px; }
.group { display: grid; gap: 12px; margin: 0; padding: 16px; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-medium); }
legend { padding: 0 6px; font: var(--md-sys-title-medium); color: var(--md-sys-color-on-surface); }
.hint { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); line-height: 1.5; }
.grid { display: grid; gap: 12px; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr)); align-items: start; }
.grid label { display: grid; gap: 6px; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.grid small { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.invalid { color: var(--md-sys-color-error) !important; }
.text-input { box-sizing: border-box; min-height: 40px; width: 100%; padding: 8px 11px; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface); color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); }
.subgroup { display: grid; gap: 8px; padding-top: 4px; }
.subgroup__title { font: var(--md-sys-title-small); color: var(--md-sys-color-on-surface); }
.preauth { display: grid; gap: 8px; padding: 12px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low, var(--md-sys-color-surface-variant)); }
.switch-row { display: flex; align-items: center; gap: 10px; min-height: 40px; font: var(--md-sys-body-medium); color: var(--md-sys-color-on-surface); }
.switch { position: relative; width: 44px; height: 24px; flex: none; padding: 0; border: 2px solid var(--md-sys-color-outline); border-radius: var(--md-sys-shape-full); background: var(--md-sys-color-surface-variant); cursor: pointer; }
.switch[data-state='checked'] { border-color: var(--md-sys-color-primary); background: var(--md-sys-color-primary); }
.switch__thumb { display: block; width: 14px; height: 14px; margin-left: 3px; border-radius: 50%; background: var(--md-sys-color-outline); transition: transform .15s ease; }
.switch__thumb[data-state='checked'] { transform: translateX(19px); background: var(--md-sys-color-on-primary); }
.radio-row { display: flex; flex-wrap: wrap; gap: 16px; }
.radio { display: inline-flex; align-items: center; gap: 8px; font: var(--md-sys-body-medium); color: var(--md-sys-color-on-surface); cursor: pointer; }
.radio__item { display: inline-grid; place-items: center; width: 20px; height: 20px; padding: 0; border: 2px solid var(--md-sys-color-outline); border-radius: 50%; background: transparent; cursor: pointer; }
.radio__item[data-state='checked'] { border-color: var(--md-sys-color-primary); }
.radio__dot { display: block; width: 10px; height: 10px; border-radius: 50%; background: var(--md-sys-color-primary); }
.checks { display: flex; flex-wrap: wrap; gap: 12px; }
.check { display: inline-flex; align-items: center; gap: 6px; font: var(--md-sys-body-medium); }
.text-button { justify-self: start; padding: 0; border: 0; background: transparent; color: var(--md-sys-color-primary); font: var(--md-sys-label-large); cursor: pointer; }
.text-button.danger { color: var(--md-sys-color-error); }
.outlined-button { justify-self: start; min-height: 36px; padding: 0 16px; border: 1px solid var(--md-sys-color-outline); border-radius: var(--md-sys-shape-full); background: transparent; color: var(--md-sys-color-primary); font: var(--md-sys-label-large); cursor: pointer; }
.switch:focus-visible, .radio__item:focus-visible { outline: 2px solid var(--md-sys-color-primary); outline-offset: 2px; }
</style>
