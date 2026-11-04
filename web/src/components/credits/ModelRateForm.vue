<template>
  <section
    class="model-rates"
    aria-labelledby="model-rates-heading"
    data-testid="model-rate-section"
  >
    <div class="section-heading">
      <div>
        <h4 id="model-rates-heading">
          模型 token 费率
        </h4>
        <p>
          一个模型需要输入、输出、缓存读取、缓存写入四类 CREDITS 费率，且四类使用同一个单次调用预占上限，模型调用才能开始。缺任何一类或上限不一致都会拒绝调用，不按免费处理，也不按外币价格换算。
        </p>
      </div>
    </div>

    <ul
      v-if="readiness.length"
      class="readiness-list"
      aria-label="模型费率就绪情况"
      data-testid="model-rate-readiness"
    >
      <li
        v-for="item in readiness"
        :key="item.modelBinding"
        class="readiness-row"
        :data-model="item.modelBinding"
      >
        <strong>{{ item.modelBinding }}</strong>
        <span
          v-if="item.state.kind === 'ready'"
          class="state-chip state-chip--ready"
        >可调用 · 单次最多预占 {{ item.state.maxPreauthorization }} CREDITS</span>
        <span
          v-else-if="item.state.kind === 'missing'"
          class="state-chip state-chip--blocked"
        >调用会被拒绝：缺少{{ item.state.missing.map((unit) => modelTokenUnitLabels[unit]).join('、') }}费率</span>
        <span
          v-else
          class="state-chip state-chip--blocked"
        >调用会被拒绝：四类费率的单次预占上限不一致（{{ item.state.caps.join(' / ') }} CREDITS）</span>
      </li>
    </ul>
    <p
      v-if="readiness.length"
      class="field-note"
    >
      按当前费率列表推断现行版本是否齐全；实际是否放行以调用时服务端判定为准。
    </p>

    <form
      class="model-rate-form"
      data-testid="model-rate-form"
      @submit.prevent="submit"
    >
      <label>
        <span>模型标识</span>
        <input
          v-model.trim="modelBinding"
          class="text-input"
          aria-label="模型标识"
          maxlength="256"
          placeholder="网关实际调用的模型名称"
          :disabled="locked"
          required
        >
      </label>
      <label>
        <span>单次调用最多预占（CREDITS）</span>
        <input
          v-model="maxPreauthorization"
          class="text-input"
          aria-label="单次调用最多预占"
          inputmode="decimal"
          placeholder="例如 2 或 0.5"
          :disabled="locked"
          required
        >
      </label>
      <label
        v-for="unit in modelTokenUnits"
        :key="unit"
      >
        <span>{{ modelTokenUnitLabels[unit] }}单价（CREDITS / 百万 token）</span>
        <input
          v-model="prices[unit]"
          class="text-input"
          :aria-label="`${modelTokenUnitLabels[unit]} token 单价`"
          inputmode="decimal"
          placeholder="例如 3 或 0.300000"
          :disabled="locked"
          required
        >
      </label>
      <label>
        <span>生效时间</span>
        <AppDateTimePicker
          v-model="effectiveFrom"
          aria-label="模型费率生效时间"
          :disabled="locked"
          required
        />
      </label>
      <label>
        <span>结束时间（可选）</span>
        <AppDateTimePicker
          v-model="effectiveUntil"
          aria-label="模型费率结束时间"
          :disabled="locked"
        />
      </label>

      <p
        v-if="validationError && touched"
        class="field-note warning-text"
        role="alert"
        data-testid="model-rate-error"
      >
        {{ validationError }}
      </p>
      <div
        v-else-if="!validationError"
        class="impact"
        role="status"
        aria-live="polite"
        data-testid="model-rate-impact"
      >
        <p>
          每次模型调用开始前冻结 {{ canonicalCap }} CREDITS；结束后按实际 token 扣收并释放剩余部分。实际费用超过上限的部分由平台承担，不会形成项目欠款。用量未知时保留待核实，首次未知起 1 小时内未核实即释放预占，迟到的费用由平台承担。
        </p>
        <ul>
          <li
            v-for="line in impactLines"
            :key="line.unit"
          >
            {{ line.text }}
          </li>
        </ul>
        <p
          v-if="zeroPriceUnits.length"
          class="warning-text"
        >
          {{ zeroPriceUnits.map((unit) => modelTokenUnitLabels[unit]).join('、') }}单价为 0：这些 token 将明确不收取 Credits。请确认这是运营定价，而不是遗漏。
        </p>
      </div>

      <div
        v-if="batch"
        class="batch-progress"
        role="status"
        data-testid="model-rate-progress"
      >
        <strong>已创建 {{ createdCount }} / {{ modelTokenUnits.length }} 类费率</strong>
        <ul>
          <li
            v-for="request in batch.requests"
            :key="request.unit"
          >
            {{ modelTokenUnitLabels[request.unit] }}：{{ isCreated(request) ? '已创建' : '未创建' }}
          </li>
        </ul>
        <small v-if="createdCount < modelTokenUnits.length">
          在四类都创建之前，这个模型的新版本不完整，生效后调用会被拒绝。继续创建会沿用已填写的内容，已创建的类别不会重复提交。
        </small>
      </div>

      <div class="form-actions">
        <button
          type="submit"
          class="filled-button"
          :disabled="!canSubmit"
        >
          {{ batch ? `继续创建剩余 ${modelTokenUnits.length - createdCount} 类费率` : '创建四类模型费率' }}
        </button>
        <button
          v-if="batch"
          type="button"
          class="text-button"
          :disabled="busy || submitting"
          @click="batch = null"
        >
          放弃未完成的部分
        </button>
      </div>
    </form>
  </section>
</template>

<script setup lang="ts">
import { computed, reactive, ref, toRaw } from 'vue'
import AppDateTimePicker from '@/components/form/AppDateTimePicker.vue'
import type { CreateResourceRateRequestSchema, ResourceRateSchema } from '@/generated/contracts'
import { formatTimestamp } from '@/utils/format'
import {
  canonicalPositiveCredits,
  canonicalRateAmount,
  localDateTimeValue,
  modelBindingsOf,
  modelRateReadiness,
  modelRateRequests,
  modelTokenUnitLabels,
  modelTokenUnits,
  rateWindowError,
  supersededRate,
  toUtcTimestamp,
  tokensCoveredByCap,
  type ModelTokenUnit,
} from '@/utils/resourceRates'

type ModelRateRequest = CreateResourceRateRequestSchema & { unit: ModelTokenUnit }

const props = defineProps<{
  /** The authoritative rate list, or null while it is unavailable. */
  rates: ResourceRateSchema[] | null
  now: number
  busy: boolean
  /** Creates one rate version through the shared rate composable; resolves true when Resource accepted it. */
  create: (input: CreateResourceRateRequestSchema) => Promise<boolean>
}>()

const modelBinding = ref('')
const maxPreauthorization = ref('')
const prices = reactive<Record<ModelTokenUnit, string>>({
  llm_input_token: '',
  llm_output_token: '',
  llm_cache_read_token: '',
  llm_cache_write_token: '',
})
const effectiveFrom = ref(localDateTimeValue(new Date(Date.now() + 5 * 60_000)))
const effectiveUntil = ref('')
const submitting = ref(false)
/** One four-dimension submission; frozen until every dimension is created or the rest is abandoned. */
const batch = ref<{ requests: ModelRateRequest[]; accepted: Set<ModelTokenUnit> } | null>(null)
const locked = computed(() => batch.value !== null)

const readiness = computed(() => (props.rates ? modelBindingsOf(props.rates) : [])
  .map((binding) => ({ modelBinding: binding, state: modelRateReadiness(props.rates!, binding, props.now) })))

const canonicalCap = computed(() => canonicalPositiveCredits(maxPreauthorization.value))
const draftRequests = computed(() => modelRateRequests({
  modelBinding: modelBinding.value,
  maxPreauthorization: maxPreauthorization.value,
  prices,
  effectiveFrom: toUtcTimestamp(effectiveFrom.value),
  effectiveUntil: effectiveUntil.value ? toUtcTimestamp(effectiveUntil.value) : null,
}))
const validationError = computed(() => {
  if (batch.value) return null
  return rateWindowError(effectiveFrom.value, effectiveUntil.value, props.now)
    ?? ('error' in draftRequests.value ? draftRequests.value.error : null)
})
/** An untouched form only disables submit; reasons appear once the administrator starts entering a model. */
const touched = computed(() => Boolean(modelBinding.value || maxPreauthorization.value.trim() || modelTokenUnits.some((unit) => prices[unit].trim())))
const zeroPriceUnits = computed(() => modelTokenUnits.filter((unit) => canonicalRateAmount(prices[unit].trim()) === '0.000000'))
const impactLines = computed(() => modelTokenUnits.map((unit) => {
  const price = canonicalRateAmount(prices[unit].trim())!
  const covered = tokensCoveredByCap(canonicalCap.value!, price)
  const replaced = props.rates ? supersededRate(props.rates, { unit, modelBinding: modelBinding.value }) : null
  const coverage = covered === null ? '单价为 0，不受上限约束' : `上限约覆盖 ${covered.toLocaleString('en-US')} 个 token`
  const replacement = replaced
    ? `；将在 ${formatTimestamp(toUtcTimestamp(effectiveFrom.value)!)} 结束现行版本 ${replaced.revision}（${replaced.unitPrice.amount} ${replaced.unitPrice.currency}，上限 ${replaced.maxPreauthorization ?? '未设置'} CREDITS）`
    : '；这个模型的该类别目前没有未结束的版本'
  return { unit, text: `${modelTokenUnitLabels[unit]}：${price} CREDITS / 百万 token，${coverage}${replacement}。` }
}))

/** Accepted in this page, or already present in the authoritative list with the exact submitted terms. */
function isCreated(request: ModelRateRequest): boolean {
  if (batch.value?.accepted.has(request.unit)) return true
  return (props.rates ?? []).some((rate) => rate.unit === request.unit
    && rate.modelBinding === request.modelBinding
    && rate.maxPreauthorization === request.maxPreauthorization
    && rate.unitPrice.amount === request.unitPrice.amount
    && rate.unitPrice.currency === request.unitPrice.currency
    && Date.parse(rate.effectiveFrom) === Date.parse(request.effectiveFrom)
    && (rate.effectiveUntil ? Date.parse(rate.effectiveUntil) : null) === (request.effectiveUntil ? Date.parse(request.effectiveUntil) : null))
}
const createdCount = computed(() => batch.value ? batch.value.requests.filter(isCreated).length : 0)
const canSubmit = computed(() => !props.busy && !submitting.value && (batch.value !== null || validationError.value === null))

async function submit() {
  if (!canSubmit.value) return
  if (!batch.value) {
    const planned = draftRequests.value
    if ('error' in planned) return
    batch.value = { requests: planned.requests, accepted: new Set() }
  }
  submitting.value = true
  try {
    for (const request of batch.value.requests) {
      if (isCreated(request)) continue
      // The composable clones its retry input; hand it the plain request, not the reactive proxy.
      if (!await props.create(toRaw(request))) return
      batch.value.accepted.add(request.unit)
    }
    batch.value = null
    for (const unit of modelTokenUnits) prices[unit] = ''
    effectiveFrom.value = localDateTimeValue(new Date(Date.now() + 5 * 60_000))
    effectiveUntil.value = ''
  } finally {
    submitting.value = false
  }
}
</script>

<style scoped>
.model-rates { display: grid; gap: var(--space-3); padding-top: var(--space-4); border-top: 1px solid var(--md-sys-color-outline-variant); }
.section-heading h4 { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-medium); }
.section-heading p, .field-note, .impact, .batch-progress { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); line-height: 1.5; }
.readiness-list, .impact ul, .batch-progress ul { display: grid; gap: var(--space-1); margin: 0; padding: 0; list-style: none; }
.readiness-row { display: flex; align-items: center; justify-content: space-between; gap: var(--space-3); padding: var(--space-2) var(--space-3); border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low); overflow-wrap: anywhere; }
.model-rate-form { display: grid; gap: var(--space-3); grid-template-columns: repeat(auto-fit, minmax(min(210px, 100%), 1fr)); align-items: end; }
.model-rate-form label { display: grid; gap: var(--space-2); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.model-rate-form > p, .impact, .batch-progress, .form-actions { grid-column: 1 / -1; }
.impact, .batch-progress { display: grid; gap: var(--space-2); }
.form-actions { display: flex; gap: var(--space-2); }
.warning-text { color: var(--md-sys-color-error); }
.state-chip { display: inline-flex; white-space: nowrap; padding: var(--space-1) var(--space-2); border-radius: var(--md-sys-shape-full); background: var(--md-sys-color-surface-variant); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); }
.state-chip--ready { background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); }
.state-chip--blocked { background: var(--md-sys-color-error-container); color: var(--md-sys-color-on-error-container); }
@media (max-width: 620px) { .readiness-row { align-items: flex-start; flex-direction: column; } .state-chip { white-space: normal; } }
</style>
