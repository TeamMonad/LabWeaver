<template>
  <section
    class="estimate-card md-card"
    aria-labelledby="cost-estimate-heading"
    data-testid="project-resource-cost-estimate"
  >
    <div class="section-heading">
      <div>
        <h3 id="cost-estimate-heading">
          资源 CREDITS 估价
        </h3>
        <p>
          调用服务端 <code>estimateProjectResourceCost</code>；计算与存储半开区间相互独立。金额取服务端六位 CREDITS 字符串，页面不重算合计；缺费率、缺目录或过时报价会显示明确诊断，不会显示为零或免费。
        </p>
      </div>
    </div>

    <form
      class="estimate-form"
      @submit.prevent="submitEstimate"
    >
      <div class="two-columns">
        <label>
          <span>CPU（millicores）</span>
          <input
            v-model.number="cpuMillicores"
            class="text-input"
            type="number"
            min="1"
            required
          >
        </label>
        <label>
          <span>内存（GiB）</span>
          <input
            v-model.number="memoryGiB"
            class="text-input"
            type="number"
            min="1"
            required
          >
        </label>
        <label>
          <span>存储（GiB）</span>
          <input
            v-model.number="storageGiB"
            class="text-input"
            type="number"
            min="1"
            required
          >
        </label>
        <label>
          <span>GPU 类别（可选）</span>
          <input
            v-model="gpuClass"
            class="text-input"
            type="text"
            placeholder="留空表示不申请 GPU"
          >
        </label>
        <label v-if="gpuClass.trim()">
          <span>GPU 数量</span>
          <input
            v-model.number="gpuCount"
            class="text-input"
            type="number"
            min="1"
            required
          >
        </label>
        <label>
          <span>计算时长（小时）</span>
          <input
            v-model.number="computeHours"
            class="text-input"
            type="number"
            min="1"
            max="720"
            required
          >
        </label>
        <label>
          <span>存储时长（小时）</span>
          <input
            v-model.number="storageHours"
            class="text-input"
            type="number"
            min="1"
            max="8760"
            required
          >
        </label>
      </div>
      <small class="field-note">
        默认从当前时刻起算半开区间；存储可长于计算。预占由 Environment/Task 配置 owner 在内部入口完成，浏览器不会调用 estimated-holds。
      </small>
      <button
        type="submit"
        class="filled-button"
        :disabled="!canSubmit || cost.acting || !projectId"
      >
        请求服务端估价
      </button>
    </form>

    <AsyncStateView
      :state="cost.estimate"
      empty-text="尚未请求估价。"
      @retry="submitEstimate"
    >
      <template #success="{ data }">
        <div
          class="estimate-summary"
          data-testid="cost-estimate-summary"
        >
          <div>
            <span>服务端合计</span>
            <strong>{{ data.total.amount }} {{ data.total.currency }}</strong>
          </div>
          <div>
            <span>明细行数</span>
            <strong>{{ data.lines.length }}</strong>
          </div>
        </div>
        <small class="field-note">
          合计直接显示服务端返回值；下面数量仅展示 quantity 与 quantityDivisor，不用于本地加总金额。
        </small>
        <ul
          class="line-list"
          aria-label="估价明细"
        >
          <li
            v-for="(line, index) in data.lines"
            :key="`${line.rateId}-${line.rateRevision}-${index}`"
            class="line-row"
          >
            <div class="line-main">
              <strong>{{ billingUnitLabel(line.unit) }} · {{ line.amount.amount }} {{ line.amount.currency }}</strong>
              <small>
                单价 {{ line.unitPrice.amount }} {{ line.unitPrice.currency }}
                / {{ line.unitQuantity }} · 数量 {{ formatEstimateQuantity(line) }}
              </small>
              <small>费率 {{ line.rateId }} · revision {{ line.rateRevision }}</small>
            </div>
          </li>
        </ul>
        <details class="advanced-details">
          <summary>查看估价输入回显</summary>
          <small>项目：{{ data.projectId }}</small>
          <small>计算：{{ formatTimestamp(data.input.computeFrom) }} → {{ formatTimestamp(data.input.computeUntil) }}</small>
          <small>存储：{{ formatTimestamp(data.input.storageFrom) }} → {{ formatTimestamp(data.input.storageUntil) }}</small>
          <small>
            规格：CPU {{ data.input.resources.cpuMillicores }}m ·
            内存 {{ data.input.resources.memoryBytes }} B ·
            存储 {{ data.input.resources.storageBytes }} B
            <template v-if="data.input.resources.gpu">
              · GPU {{ data.input.resources.gpu.class }} × {{ data.input.resources.gpu.count }}
            </template>
          </small>
        </details>
      </template>
    </AsyncStateView>
  </section>
</template>

<script setup lang="ts">
import { computed, reactive, ref, toRef, watch } from 'vue'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import {
  billingUnitLabel,
  formatEstimateQuantity,
  hoursFromNowInterval,
  useProjectResourceCostEstimate,
} from '@/composables/useProjectResourceCostEstimate'
import { formatTimestamp } from '@/utils/format'

const props = defineProps<{
  projectId: string | null
}>()

const cost = reactive(useProjectResourceCostEstimate(toRef(props, 'projectId')))
const cpuMillicores = ref(2000)
const memoryGiB = ref(4)
const storageGiB = ref(20)
const computeHours = ref(8)
const storageHours = ref(8)
const gpuClass = ref('')
const gpuCount = ref(1)

const canSubmit = computed(() => Boolean(
  props.projectId
  && Number.isInteger(cpuMillicores.value) && cpuMillicores.value > 0
  && Number.isInteger(memoryGiB.value) && memoryGiB.value > 0
  && Number.isInteger(storageGiB.value) && storageGiB.value > 0
  && Number.isInteger(computeHours.value) && computeHours.value > 0
  && Number.isInteger(storageHours.value) && storageHours.value > 0
  && (!gpuClass.value.trim() || (Number.isInteger(gpuCount.value) && gpuCount.value > 0)),
))

watch(
  () => props.projectId,
  () => {
    // Form defaults stay; previous estimate cleared by composable.
  },
)

async function submitEstimate() {
  if (!canSubmit.value) return
  const compute = hoursFromNowInterval(computeHours.value)
  const storage = hoursFromNowInterval(storageHours.value)
  const className = gpuClass.value.trim()
  await cost.run({
    computeFrom: compute.from,
    computeUntil: compute.until,
    storageFrom: storage.from,
    storageUntil: storage.until,
    resources: {
      cpuMillicores: cpuMillicores.value,
      memoryBytes: memoryGiB.value * 1024 ** 3,
      storageBytes: storageGiB.value * 1024 ** 3,
      ...(className ? { gpu: { class: className, count: gpuCount.value } } : {}),
    },
  })
}

defineExpose({ reload: () => cost.reset() })
</script>

<style scoped>
.estimate-card { display: grid; gap: 18px; padding: 20px; }
.section-heading { display: flex; align-items: flex-start; justify-content: space-between; gap: 16px; }
.section-heading h3 { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-large); }
.section-heading p { margin: 6px 0 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); line-height: 1.5; }
.estimate-form { display: grid; gap: 12px; }
.two-columns { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 10px; }
.estimate-form label { display: grid; gap: 6px; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.text-input { box-sizing: border-box; min-height: 40px; width: 100%; padding: 8px 11px; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface); color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); }
.field-note { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); line-height: 1.45; }
.estimate-summary { display: grid; gap: 10px; grid-template-columns: repeat(auto-fit, minmax(180px, 1fr)); }
.estimate-summary div { display: grid; gap: 5px; padding: 13px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low); }
.estimate-summary span { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); }
.estimate-summary strong { color: var(--md-sys-color-on-surface); font: var(--md-sys-title-medium); white-space: nowrap; }
.line-list { display: grid; gap: 8px; margin: 0; padding: 0; list-style: none; }
.line-row { padding: 14px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low); }
.line-main { display: grid; gap: 4px; }
.line-main strong { color: var(--md-sys-color-on-surface); font: var(--md-sys-title-medium); }
.line-main small { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); overflow-wrap: anywhere; }
.advanced-details { display: grid; gap: 4px; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); }
.advanced-details summary { color: var(--md-sys-color-primary); cursor: pointer; }
.filled-button { display: inline-flex; align-items: center; justify-content: center; min-height: 40px; padding: 0 17px; border-radius: var(--md-sys-shape-full); border: 1px solid var(--md-sys-color-primary); background: var(--md-sys-color-primary); color: var(--md-sys-color-on-primary); font: var(--md-sys-label-large); cursor: pointer; width: fit-content; }
.filled-button:disabled { opacity: .5; cursor: not-allowed; }
@media (max-width: 620px) {
  .two-columns, .estimate-summary { grid-template-columns: 1fr; }
}
</style>
