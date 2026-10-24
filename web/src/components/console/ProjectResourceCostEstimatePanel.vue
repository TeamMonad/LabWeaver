<template>
  <section
    class="estimate-card md-card"
    aria-labelledby="cost-estimate-heading"
    data-testid="project-resource-cost-estimate"
  >
    <div class="section-heading">
      <div>
        <h3 id="cost-estimate-heading">
          费用估价
        </h3>
        <p>
          按当前费率估算资源申请和镜像构建的 CREDITS 上限。估价不预占资金也不占用容量；缺少费率或 GPU 类型不可用时估价会失败，不会显示为零或免费。
        </p>
      </div>
    </div>

    <form
      class="estimate-form"
      aria-labelledby="resource-estimate-heading"
      @submit.prevent="submitEstimate"
    >
      <h4 id="resource-estimate-heading">
        资源申请
      </h4>
      <div class="two-columns">
        <label>
          <span>CPU（m）</span>
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
          <span>GPU 类型（可选）</span>
          <AppSelect
            v-model="gpuClass"
            aria-label="估价 GPU 类型"
            :disabled="gpuUnavailableReason !== null"
          >
            <option value="">不申请 GPU</option>
            <option
              v-for="name in gpuClasses"
              :key="name"
              :value="name"
            >{{ name }}</option>
          </AppSelect>
          <small
            v-if="gpuUnavailableReason"
            class="field-note warning-text"
            role="status"
            data-testid="estimate-gpu-unavailable"
          >{{ gpuUnavailableReason }}</small>
        </label>
        <label v-if="gpuClass">
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
          <span>存储保留时长（小时）</span>
          <input
            v-model.number="storageHours"
            class="text-input"
            type="number"
            min="1"
            max="8760"
            required
          >
          <small class="field-note">停止计算后保留的磁盘按存储时长单独计价，可以长于计算时长。</small>
        </label>
      </div>
      <button
        type="submit"
        class="outlined-button"
        :disabled="!canSubmit || cost.acting"
      >
        估价
      </button>
    </form>

    <AsyncStateView
      :state="cost.estimate"
      empty-text=""
      @retry="submitEstimate"
    >
      <template #success="{ data }">
        <CostEstimateResult
          :total="data.total"
          :lines="data.lines"
        />
      </template>
    </AsyncStateView>

    <form
      class="estimate-form"
      aria-labelledby="build-estimate-heading"
      data-testid="build-cost-estimate-form"
      @submit.prevent="submitBuildEstimate"
    >
      <h4 id="build-estimate-heading">
        镜像构建
      </h4>
      <p class="field-note">
        构建开始前按所选构建器的费率和最长构建时长预占 Credits；没有匹配费率时新构建会被拒绝。
      </p>
      <div class="two-columns">
        <label>
          <span>构建器</span>
          <AppSelect
            v-model="builderBinding"
            aria-label="构建器"
            :disabled="builderBindings.length === 0"
          >
            <option value="">选择构建器</option>
            <option
              v-for="binding in builderBindings"
              :key="binding"
              :value="binding"
            >{{ binding }}</option>
          </AppSelect>
          <small
            v-if="builderBindings.length === 0"
            class="field-note warning-text"
            role="status"
            data-testid="build-estimate-no-rate"
          >还没有镜像构建费率，无法估价，新的构建也会被拒绝。请先在上方创建“镜像构建秒”费率。</small>
        </label>
        <label>
          <span>最长构建时长（分钟）</span>
          <input
            v-model.number="buildMinutes"
            class="text-input"
            type="number"
            min="1"
            max="1440"
            required
          >
        </label>
      </div>
      <button
        type="submit"
        class="outlined-button"
        :disabled="!canSubmitBuild || build.acting"
      >
        估价
      </button>
    </form>

    <AsyncStateView
      :state="build.estimate"
      empty-text=""
      @retry="submitBuildEstimate"
    >
      <template #success="{ data }">
        <CostEstimateResult
          :total="data.total"
          :lines="data.lines"
        />
      </template>
    </AsyncStateView>
  </section>
</template>

<script setup lang="ts">
import { computed, reactive, ref, toRef, watch } from 'vue'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import CostEstimateResult from '@/components/console/CostEstimateResult.vue'
import AppSelect from '@/components/form/AppSelect.vue'
import {
  hoursFromNowInterval,
  useProjectBuildCostEstimate,
  useProjectResourceCostEstimate,
} from '@/composables/useProjectResourceCostEstimate'

const props = defineProps<{
  projectId: string | null
  /** GPU classes from the active catalog. */
  gpuClasses: readonly string[]
  /** Why GPU cannot be estimated right now (catalog failed, loading or empty); null when selectable. */
  gpuUnavailableReason: string | null
  /** Builders that currently have a build_second rate. */
  builderBindings: readonly string[]
}>()

const projectIdRef = toRef(props, 'projectId')
const cost = reactive(useProjectResourceCostEstimate(projectIdRef))
const build = reactive(useProjectBuildCostEstimate(projectIdRef))
const cpuMillicores = ref(2000)
const memoryGiB = ref(4)
const storageGiB = ref(20)
const computeHours = ref(8)
const storageHours = ref(8)
const gpuClass = ref('')
const gpuCount = ref(1)
const builderBinding = ref('')
const buildMinutes = ref(30)

const positiveInteger = (value: number) => Number.isInteger(value) && value > 0

const canSubmit = computed(() => Boolean(
  props.projectId
  && [cpuMillicores.value, memoryGiB.value, storageGiB.value, computeHours.value, storageHours.value].every(positiveInteger)
  && (!gpuClass.value || positiveInteger(gpuCount.value)),
))
const canSubmitBuild = computed(() => Boolean(
  props.projectId && props.builderBindings.includes(builderBinding.value) && positiveInteger(buildMinutes.value),
))

watch(() => props.gpuClasses, (classes) => {
  if (gpuClass.value && !classes.includes(gpuClass.value)) gpuClass.value = ''
})
watch(() => props.builderBindings, (bindings) => {
  if (builderBinding.value && !bindings.includes(builderBinding.value)) builderBinding.value = ''
  if (!builderBinding.value && bindings.length === 1) builderBinding.value = bindings[0]!
}, { immediate: true })
watch([cpuMillicores, memoryGiB, storageGiB, computeHours, storageHours, gpuClass, gpuCount], () => cost.reset())
watch([builderBinding, buildMinutes], () => build.reset())

async function submitEstimate() {
  if (!canSubmit.value) return
  const compute = hoursFromNowInterval(computeHours.value)
  const storage = hoursFromNowInterval(storageHours.value)
  await cost.run({
    computeFrom: compute.from,
    computeUntil: compute.until,
    storageFrom: storage.from,
    storageUntil: storage.until,
    resources: {
      cpuMillicores: cpuMillicores.value,
      memoryBytes: memoryGiB.value * 1024 ** 3,
      storageBytes: storageGiB.value * 1024 ** 3,
      ...(gpuClass.value ? { gpu: { class: gpuClass.value, count: gpuCount.value } } : {}),
    },
  })
}

async function submitBuildEstimate() {
  if (!canSubmitBuild.value) return
  const from = new Date()
  from.setMilliseconds(0)
  await build.run({
    builderBinding: builderBinding.value,
    from: from.toISOString(),
    maxDurationMilliseconds: buildMinutes.value * 60_000,
  })
}

defineExpose({ reload: () => { cost.reset(); build.reset() } })
</script>

<style scoped>
.estimate-card { display: grid; gap: var(--space-4); padding: var(--space-5); }
.section-heading h3 { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-large); }
.section-heading p { margin: var(--space-1) 0 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); line-height: 1.5; }
.estimate-form { display: grid; gap: var(--space-3); padding-top: var(--space-4); border-top: 1px solid var(--md-sys-color-outline-variant); }
.estimate-form h4 { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-medium); }
.estimate-form > .outlined-button { width: fit-content; }
.two-columns { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: var(--space-3); }
.estimate-form label { display: grid; gap: var(--space-1); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.field-note { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); line-height: 1.45; }
.warning-text { color: var(--md-sys-color-error); }
@media (max-width: 620px) {
  .two-columns { grid-template-columns: 1fr; }
}
</style>
