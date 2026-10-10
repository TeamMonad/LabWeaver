<template>
  <div class="finance-page">
    <header class="page-header">
      <div>
        <h2>费率、预算与费用</h2>
        <p class="page-subtitle">
          全局费率无需选择项目；项目预算和费用须有相应权限。金额用于核算，不代表支付。
        </p>
      </div>
      <button
        type="button"
        class="icon-button"
        aria-label="刷新预算与费用"
        :disabled="finance.acting !== null"
        @click="finance.load"
      >
        <SvgIcon
          name="refresh"
          size="sm"
          aria-hidden="true"
        />
      </button>
    </header>

    <section class="project-strip md-card">
      <label>
        <span>项目</span>
        <select
          v-model="selectedProjectId"
          class="text-input"
          :disabled="projects.projects.kind !== 'success'"
        >
          <option value="">只管理全局费率</option>
          <option
            v-for="project in projectOptions"
            :key="project.id"
            :value="project.id"
          >{{ project.name }}</option>
        </select>
      </label>
      <span
        v-if="selectedProject"
        class="project-scope"
      >{{ selectedProject.courseId ? '课程项目' : '独立科研项目' }}</span>
      <details
        v-if="selectedProject"
        class="advanced-details project-id-details"
      >
        <summary>查看项目内部标识</summary>
        <small>项目 ID：{{ selectedProject.id }}</small>
        <small v-if="selectedProject.courseId">课程 ID：{{ selectedProject.courseId }}</small>
      </details>
    </section>

    <DiagnosticBanner
      v-if="projectContextUnavailable"
      code="PROJECT_CONTEXT_UNAVAILABLE"
      message="链接中的项目不存在或你无权访问，已停止加载预算和费用。请从项目选择器重新选择。"
      :retryable="false"
      severity="warning"
    />
    <RouterLink
      v-if="projectContextUnavailable"
      class="outlined-button project-context-action"
      to="/researcher/workspaces"
    >
      打开项目与工作空间
    </RouterLink>

    <DiagnosticBanner
      v-if="finance.outcome"
      :code="finance.outcome.diagnostic.code"
      :message="finance.outcome.diagnostic.message"
      :retryable="finance.outcome.diagnostic.retryable"
      :severity="finance.outcome.kind === 'error' ? 'error' : 'info'"
      @retry="finance.load"
    />

    <DiagnosticBanner
      v-if="rates.outcome"
      :code="rates.outcome.diagnostic.code"
      :message="rates.outcome.diagnostic.message"
      :retryable="rates.outcome.diagnostic.retryable"
      :severity="rates.outcome.kind === 'error' ? 'error' : 'info'"
      @retry="rates.outcome?.operation === 'end' ? rates.retryEnd() : rates.outcome?.kind === 'error' ? rates.retryCreate() : rates.load()"
    />

    <section
      class="rates-card md-card"
      aria-labelledby="rates-heading"
    >
      <div class="section-heading">
        <div>
          <h3 id="rates-heading">
            资源费率
          </h3>
          <p>费率面向全平台。创建未来版本会在生效时间结束同维度旧版本；历史用量和费用保持原费率快照，不会重新计价。</p>
        </div>
        <button
          type="button"
          class="icon-button"
          aria-label="刷新资源费率"
          :disabled="rates.acting !== null"
          @click="rates.load"
        >
          <SvgIcon
            name="refresh"
            size="sm"
            aria-hidden="true"
          />
        </button>
      </div>
      <AsyncStateView
        :state="rates.rates"
        empty-text="还没有资源费率。未配置费率的用量会保留待计价状态；资源申请仍按目录、容量和审批规则处理。"
        @retry="rates.load"
      >
        <template #success="{ data }">
          <ul
            class="rate-list"
            aria-label="资源费率列表"
          >
            <li
              v-for="rate in data"
              :key="`${rate.id}-${rate.revision}`"
              class="rate-row"
            >
              <div class="rate-row-main">
                <div class="rate-main">
                  <strong>{{ rateLabel(rate) }}</strong>
                  <small>{{ rate.unitQuantity }} 基础单位 · {{ rate.unitPrice.amount }} {{ rate.unitPrice.currency }} · {{ formatTimestamp(rate.effectiveFrom) }} 起</small>
                  <small>基础单位：{{ rateDisplayUnits[rate.unit]?.base ?? '未知计费单位' }}</small>
                  <small>{{ equivalentRatePrice(rate.unit, rate.unitQuantity, rate.unitPrice.amount, rate.unitPrice.currency) ?? '金额或基础单位数量无效，无法换算。' }}</small>
                  <small v-if="rate.effectiveUntil">截止于 {{ formatTimestamp(rate.effectiveUntil) }}</small>
                </div>
                <div class="rate-actions">
                  <span class="state-chip">{{ rateVersionState(rate, now) }} · 版本 {{ rate.revision }}</span>
                  <button
                    v-if="!rate.effectiveUntil"
                    type="button"
                    class="outlined-button small"
                    :data-testid="`end-rate-${rate.id}`"
                    :disabled="rates.acting !== null"
                    @click="beginEndRate(rate)"
                  >
                    安排结束
                  </button>
                </div>
              </div>
              <div
                v-if="endingRateId === rate.id"
                class="rate-end-editor"
                data-testid="resource-rate-end-form"
              >
                <label>
                  <span>截止时间</span>
                  <input
                    v-model="endingEffectiveUntil"
                    class="text-input"
                    type="datetime-local"
                    aria-label="费率截止时间"
                  >
                </label>
                <p class="rate-equivalent">
                  只结束这一费率版本。截止后不再用于新的用量核算，历史账单快照保留，现有环境不会被强制停止。
                </p>
                <p
                  v-if="endDateError"
                  class="rate-equivalent warning-text"
                  role="alert"
                >
                  {{ endDateError }}
                </p>
                <div class="rate-end-actions">
                  <button
                    type="button"
                    class="filled-button"
                    :disabled="!canSubmitEndRate || rates.acting !== null"
                    @click="openEndConfirmation(rate)"
                  >
                    继续
                  </button>
                  <button
                    type="button"
                    class="text-button"
                    :disabled="rates.acting !== null"
                    @click="cancelEndRate"
                  >
                    取消
                  </button>
                </div>
              </div>
            </li>
          </ul>
        </template>
      </AsyncStateView>
      <form
        class="rate-form"
        data-testid="resource-rate-form"
        @submit.prevent="submitRate"
      >
        <label>
          <span>计费单位</span>
          <select
            v-model="rateUnit"
            class="text-input"
            aria-label="计费单位"
          >
            <option value="gpu_unit_second">GPU 分配单位秒</option>
            <option value="cpu_millicore_second">CPU 核心小时</option>
            <option value="memory_byte_second">内存 GiB 小时</option>
            <option value="storage_byte_second">存储 GiB 小时</option>
          </select>
        </label>
        <label v-if="rateUnit === 'gpu_unit_second'">
          <span>GPU 目录分配类型</span>
          <select
            v-model="rateGpuSelection"
            class="text-input"
            aria-label="GPU 目录分配类型"
            :disabled="catalog.kind !== 'success'"
            required
          >
            <option value="">选择 GPU 类型与分配模式</option>
            <option
              v-for="entry in gpuOptions"
              :key="gpuOptionKey(entry)"
              :value="gpuOptionKey(entry)"
            >{{ entry.class }} · {{ rateModeLabel(entry.mode) }}</option>
          </select>
        </label>
        <p
          v-if="rateUnit === 'gpu_unit_second'"
          class="rate-equivalent"
        >
          {{ gpuModeDescription }}
          共享单位按目录的一份时间片分配计量，不代表一张独占 GPU 的性能。
        </p>
        <DiagnosticBanner
          v-if="rateUnit === 'gpu_unit_second' && catalog.kind === 'error'"
          :code="catalog.diagnostic.code"
          :message="catalog.diagnostic.message"
          :retryable="catalog.diagnostic.retryable"
          @retry="loadCatalog"
        />
        <p
          v-if="rateUnit === 'gpu_unit_second' && catalog.kind === 'empty'"
          class="rate-equivalent"
        >
          尚无启用的 GPU 目录类型。请先在 GPU 目录配置类型与分配模式，再创建对应费率；目录创建不要求先有费率。
        </p>
        <RouterLink
          v-if="rateUnit === 'gpu_unit_second'"
          class="text-button"
          to="/admin/gpu-catalog"
        >
          管理 GPU 目录
        </RouterLink>
        <label>
          <span>单价（每 {{ rateDisplayUnits[rateUnit].label }}）</span>
          <input
            v-model="rateAmount"
            class="text-input"
            aria-label="费率单价"
            inputmode="decimal"
            pattern="(0|[1-9][0-9]*)(\.[0-9]{1,6})?"
            placeholder="例如 1 或 0.000100"
            required
          >
        </label>
        <label>
          <span>币种</span>
          <input
            v-model="rateCurrency"
            class="text-input"
            maxlength="32"
            pattern="[A-Za-z0-9_\-]{1,32}"
            required
          >
        </label>
        <label>
          <span>生效时间</span>
          <input
            v-model="rateEffectiveFrom"
            class="text-input"
            type="datetime-local"
            required
          >
        </label>
        <label>
          <span>结束时间（可选）</span>
          <input
            v-model="rateEffectiveUntil"
            class="text-input"
            type="datetime-local"
          >
        </label>
        <p
          class="rate-equivalent"
          role="status"
          aria-live="polite"
        >
          基础单位：{{ rateDisplayUnits[rateUnit].base }}。
          {{ equivalentRatePrice(rateUnit, rateUnitQuantity ?? 0, rateAmount, rateCurrency) ?? '请输入非负金额（最多六位小数）和有效的安全整数数量。' }}
          实际提交：{{ rateUnitQuantity ?? '无效' }} {{ rateDisplayUnits[rateUnit].base }}，金额 {{ canonicalRateAmount(rateAmount) ?? '无效' }} {{ rateCurrency }}。换算不改现有费率或历史费用。
        </p>
        <p
          v-if="rateDateError"
          class="rate-equivalent warning-text"
          role="alert"
        >
          {{ rateDateError }}
        </p>
        <button
          type="submit"
          class="filled-button"
          :disabled="!canSubmitRate || rates.acting !== null"
        >
          创建费率版本
        </button>
      </form>
    </section>

    <p
      v-if="!selectedProjectId"
      class="page-subtitle"
    >
      项目财务未加载。选择已授权项目后查看预算、费用和调整；全局列表中的项目不代表你拥有该项目权限。
    </p>
    <div
      v-if="selectedProjectId"
      class="finance-layout"
    >
      <section
        class="usage-card md-card"
        aria-labelledby="usage-heading"
      >
        <div class="section-heading">
          <div>
            <h3 id="usage-heading">
              项目用量
            </h3>
            <p>按环境和资源申请显示计算、存储区间及服务端实际测量结果。待结算或无法测量的记录会保留原状态，不按零费用处理。</p>
          </div>
          <button
            type="button"
            class="icon-button"
            aria-label="刷新项目用量"
            :disabled="finance.acting !== null"
            @click="finance.loadUsagePage(finance.usagePage)"
          >
            <SvgIcon
              name="refresh"
              size="sm"
              aria-hidden="true"
            />
          </button>
        </div>
        <DiagnosticBanner
          v-if="finance.usageContext.kind === 'error'"
          :code="finance.usageContext.diagnostic.code"
          :message="finance.usageContext.diagnostic.message"
          :retryable="finance.usageContext.diagnostic.retryable"
          @retry="finance.loadUsagePage(finance.usagePage)"
        />
        <AsyncStateView
          :state="finance.usage"
          empty-text="该项目暂无用量记录。"
          @retry="finance.loadUsagePage(finance.usagePage)"
        >
          <template #success="{ data }">
            <p
              v-if="data.items.length === 0"
              class="usage-empty"
              role="status"
            >
              {{ usageEmptyText(data) }}
            </p>
            <ul
              v-else
              class="usage-list"
              aria-label="项目用量列表"
            >
              <li
                v-for="record in data.items"
                :key="record.id"
                class="usage-row"
              >
                <div class="usage-main">
                  <strong>{{ usageTargetLabel(record) }}</strong>
                  <small>{{ usageKindLabel(record.kind) }} · {{ usageIntervalLabel(record) }}</small>
                  <small>状态：{{ settlementLabel(record.settlement) }}</small>
                  <span
                    v-if="record.measurement.state === 'known'"
                    class="usage-quantities"
                  >{{ usageQuantitiesLabel(record.measurement.quantities) }}</span>
                  <span
                    v-else
                    class="warning-text"
                  >无法确认用量：{{ record.measurement.reason }}</span>
                  <details class="advanced-details">
                    <summary>查看内部关联</summary>
                    <small>用量记录 ID：{{ record.id }}</small>
                    <small>事件 ID：{{ record.sourceEventId }}</small>
                    <small v-if="usageEnvironmentId(record)">环境 ID：{{ usageEnvironmentId(record) }}</small>
                    <template v-if="usageRequestId(record)">
                      <small>资源申请 ID：{{ usageRequestId(record) }}</small>
                      <small v-if="usageLeaseId(record)">租约 ID：{{ usageLeaseId(record) }}</small>
                    </template>
                  </details>
                </div>
              </li>
            </ul>
            <div
              v-if="data.items.length > 0 || data.page > 1 || data.hasMore"
              class="usage-pagination"
              aria-label="项目用量分页"
            >
              <button
                type="button"
                class="outlined-button small"
                :disabled="data.page <= 1 || finance.usage.kind === 'loading'"
                @click="finance.loadUsagePage(data.page - 1)"
              >
                上一页
              </button>
              <span>第 {{ data.page }} 页</span>
              <button
                type="button"
                class="outlined-button small"
                :disabled="!data.hasMore || finance.usage.kind === 'loading'"
                @click="finance.loadUsagePage(data.page + 1)"
              >
                下一页
              </button>
            </div>
          </template>
        </AsyncStateView>
      </section>
      <section
        class="budget-card md-card"
        aria-labelledby="budget-heading"
      >
        <div class="section-heading">
          <div>
            <h3 id="budget-heading">
              项目预算
            </h3>
            <p>预算和已花费会按当前项目实时更新。</p>
          </div>
        </div>
        <AsyncStateView
          :state="finance.budget"
          empty-text="该项目还没有预算记录。"
          @retry="finance.load"
        >
          <template #success="{ data }">
            <div
              v-if="budgetNotice?.warningReached"
              data-testid="budget-threshold-warning"
            >
              <DiagnosticBanner
                code="RESOURCE_BUDGET_WARNING_THRESHOLD_REACHED"
                severity="warning"
                :retryable="false"
                :message="`已花费 ${data.spent.amount} ${data.spent.currency}，已达到提醒阈值 ${data.warningAt.amount} ${data.warningAt.currency}。这是预算提醒，不会自动停止已批准的计划或资源。`"
              />
            </div>
            <div
              v-if="budgetNotice?.limitReached"
              data-testid="budget-limit-warning"
            >
              <DiagnosticBanner
                code="RESOURCE_BUDGET_LIMIT_REACHED"
                severity="warning"
                :retryable="false"
                :message="`已花费 ${data.spent.amount} ${data.spent.currency}，已达到预算上限 ${data.limit.amount} ${data.limit.currency}。这只是预算提醒，不会自动停止已批准的计划或资源。`"
              />
            </div>
            <div class="budget-summary">
              <div><span>已花费</span><strong>{{ data.spent.amount }} {{ data.spent.currency }}</strong></div>
              <div><span>预算上限</span><strong>{{ data.limit.amount }} {{ data.limit.currency }}</strong></div>
              <div><span>提醒阈值</span><strong>{{ data.warningAt.amount }} {{ data.warningAt.currency }}</strong></div>
            </div>
            <small class="updated-note">更新于 {{ formatTimestamp(data.updatedAt) }}</small>
          </template>
        </AsyncStateView>
        <form
          v-if="budgetEditable"
          class="budget-form"
          @submit.prevent="saveBudget"
        >
          <label>
            <span>币种</span>
            <input
              v-model="budgetCurrency"
              class="text-input"
              maxlength="32"
              pattern="[A-Za-z0-9_\-]{1,32}"
              required
              :readonly="finance.budget.kind === 'success'"
            >
          </label>
          <label>
            <span>预算上限（{{ budgetCurrency || '币种' }}）</span>
            <input
              v-model="limitAmount"
              class="text-input"
              inputmode="decimal"
              pattern="(0|[1-9][0-9]*)\.[0-9]{6}"
              required
            >
          </label>
          <label>
            <span>提醒阈值（{{ budgetCurrency || '币种' }}）</span>
            <input
              v-model="warningAmount"
              class="text-input"
              inputmode="decimal"
              pattern="(0|[1-9][0-9]*)\.[0-9]{6}"
              required
            >
          </label>
          <button
            type="submit"
            class="filled-button"
            :disabled="!canSaveBudget || finance.acting !== null"
          >
            {{ finance.budget.kind === 'empty' ? '创建预算' : '保存预算' }}
          </button>
        </form>
      </section>

      <section
        class="charges-card md-card"
        aria-labelledby="charges-heading"
      >
        <div class="section-heading">
          <div>
            <h3 id="charges-heading">
              费用明细
            </h3>
            <p>未知或未结算的用量保留原状态，不会显示为零。</p>
          </div>
        </div>
        <AsyncStateView
          :state="finance.charges"
          empty-text="该项目暂无费用记录。"
          @retry="finance.load"
        >
          <template #success="{ data }">
            <ul class="charge-list">
              <li
                v-for="charge in data"
                :key="charge.id"
                class="charge-row"
              >
                <div class="charge-main">
                  <strong>{{ chargeTargetLabel(charge) }} · {{ charge.total.amount }} {{ charge.total.currency }}</strong>
                  <small v-if="chargeUsage(charge)">{{ usageKindLabel(chargeUsage(charge)!.kind) }} · {{ usageIntervalLabel(chargeUsage(charge)!) }}</small>
                  <small v-else>关联用量详情尚未加载，请查看项目用量列表。</small>
                  <small>{{ formatTimestamp(charge.createdAt) }} · {{ charge.lines.length }} 个计费项</small>
                  <ul
                    class="charge-line-list"
                    aria-label="费用计算明细"
                  >
                    <li
                      v-for="line in charge.lines"
                      :key="`${line.rateId}-${line.rateRevision}-${line.unit}`"
                      class="charge-line"
                    >
                      <span>{{ billingUnitLabel(line.unit) }}</span>
                      <small>{{ line.quantity }} / {{ line.unitQuantity }} 基础单位 · 费率版本 {{ line.rateRevision }} · 单价 {{ line.unitPrice.amount }} {{ line.unitPrice.currency }}</small>
                      <strong>{{ line.amount.amount }} {{ line.amount.currency }}</strong>
                    </li>
                  </ul>
                  <small
                    v-if="charge.settlement !== 'settled'"
                    class="warning-text"
                  >
                    当前费用{{ charge.settlement === 'pending' ? '待结算' : '未结算' }}，金额可能继续变化。
                  </small>
                  <details class="advanced-details">
                    <summary>查看费用关联详情</summary>
                    <small>费用记录 ID：{{ charge.id }}</small>
                    <small>用量记录 ID：{{ charge.usageRecordId }}</small>
                    <small v-if="charge.adjustmentOf">调整自：{{ charge.adjustmentOf }}</small>
                    <small v-if="charge.adjustedBy">调整人：{{ charge.adjustedBy }}</small>
                    <small v-if="charge.diagnosticCode">诊断代码：{{ charge.diagnosticCode }}</small>
                    <small v-if="charge.adjustmentReason">调整原因：{{ charge.adjustmentReason }}</small>
                  </details>
                </div>
                <div class="charge-actions">
                  <span
                    class="state-chip"
                    :class="`state-chip--${charge.settlement}`"
                  >{{ settlementLabel(charge.settlement) }}</span>
                  <button
                    type="button"
                    class="outlined-button small"
                    @click="selectCharge(charge)"
                  >
                    调整
                  </button>
                </div>
              </li>
            </ul>
          </template>
        </AsyncStateView>

        <form
          v-if="selectedCharge"
          class="adjustment-form"
          @submit.prevent="submitAdjustment"
        >
          <div class="section-heading section-heading--compact">
            <div>
              <h4>记录费用调整</h4>
              <p>调整会追加新记录并保留原费用，不会覆盖历史金额。</p>
            </div>
            <button
              type="button"
              class="text-button"
              @click="selectedChargeId = ''"
            >
              取消
            </button>
          </div>
          <small>目标费用：{{ chargeTargetLabel(selectedCharge) }} · 原金额 {{ selectedCharge.total.amount }} {{ selectedCharge.total.currency }}</small>
          <details class="advanced-details">
            <summary>查看费用内部标识</summary>
            <small>费用记录 ID：{{ selectedCharge.id }}</small>
            <small>用量记录 ID：{{ selectedCharge.usageRecordId }}</small>
          </details>
          <div class="two-columns">
            <label>
              <span>调整金额（可为负）</span>
              <input
                v-model="adjustmentAmount"
                class="text-input"
                inputmode="decimal"
                pattern="-?(0|[1-9][0-9]*)\.[0-9]{6}"
                placeholder="0.000000"
                required
              >
            </label>
            <label>
              <span>币种</span>
              <input
                :value="selectedCharge.total.currency"
                class="text-input"
                readonly
              >
            </label>
          </div>
          <label>
            <span>调整原因</span>
            <textarea
              v-model="adjustmentReason"
              class="text-input"
              rows="2"
              maxlength="500"
              required
            />
          </label>
          <button
            type="submit"
            class="filled-button"
            :disabled="!canSubmitAdjustment || finance.acting !== null"
          >
            记录调整
          </button>
        </form>
      </section>
    </div>
    <ConfirmDialog
      :open="endConfirmation !== null"
      title="安排结束全平台费率？"
      :description="endConfirmationDescription"
      confirm-text="确认安排结束"
      cancel-text="返回修改"
      severity="warning"
      @confirm="confirmEndRate"
      @cancel="endConfirmation = null"
    />
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, onScopeDispose, ref, watch } from 'vue'
import { RouterLink, useRoute, useRouter } from 'vue-router'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import ConfirmDialog from '@/components/common/ConfirmDialog.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import SvgIcon from '@/components/common/SvgIcon.vue'
import { useProjectResourceFinance, type ResourceCharge } from '@/composables/useProjectResourceFinance'
import { canonicalRateAmount, equivalentRatePrice, rateDisplayUnits, rateVersionState } from '@/utils/resourceRates'
import { listResourceGpuCatalog } from '@/generated/contracts'
import { extractProblemDetails, makeDiagnostic, type AsyncState } from '@/types/async'
import { useResourceRates } from '@/composables/useResourceRates'
import { useProjects } from '@/composables/useProjects'
import { formatTimestamp } from '@/utils/format'
import type {
  GpuAllocationMode,
  GpuCatalogEntrySchema,
  ResourceBillingUnit,
  ResourceRateSchema,
  ResourceUsagePageSchema,
  ResourceUsageQuantities,
  ResourceUsageRecord,
} from '@/generated/contracts'

const projects = useProjects()
const route = useRoute()
const router = useRouter()
const projectOptions = computed(() => projects.projects.kind === 'success' ? projects.projects.data : [])
const routeProjectId = computed(() => {
  const value = route.query.projectId
  const projectId = Array.isArray(value) ? value[0] : value
  return typeof projectId === 'string' ? projectId : undefined
})
const projectContextUnavailable = computed(() => Boolean(
  routeProjectId.value
  && projects.projects.kind === 'success'
  && !projectOptions.value.some((project) => project.id === routeProjectId.value),
))
const selectedProjectId = computed<string | null>({
  get: () => routeProjectId.value
    ? projects.projects.kind === 'success' && !projectContextUnavailable.value ? routeProjectId.value : null
    : null,
  set: (projectId) => {
    if (projectId && projectId !== projects.selectedProjectId) projects.select(projectId)
    if (route.query.projectId !== projectId) {
      void router.replace({ query: { ...route.query, projectId: projectId || undefined } })
    }
  },
})
const selectedProject = computed(() => projectOptions.value.find((project) => project.id === selectedProjectId.value) ?? null)
const finance = useProjectResourceFinance(selectedProjectId)
const budgetCurrency = ref('USD')
const limitAmount = ref('0.000000')
const warningAmount = ref('0.000000')
const selectedChargeId = ref('')
const adjustmentAmount = ref('0.000000')
const adjustmentReason = ref('')
const rates = useResourceRates()
const rateUnit = ref<ResourceBillingUnit>('gpu_unit_second')
const rateGpuSelection = ref('')
const rateUnitQuantity = computed(() => Number(rateDisplayUnits[rateUnit.value].quantity))
const rateAmount = ref('')
const rateCurrency = ref('USD')
const rateEffectiveFrom = ref(localDateTimeValue(new Date(Date.now() + 5 * 60_000)))
const rateEffectiveUntil = ref('')
const now = ref(Date.now())
const clock = setInterval(() => { now.value = Date.now() }, 30_000)
onScopeDispose(() => clearInterval(clock))
const endingRateId = ref<string | null>(null)
const endingEffectiveUntil = ref('')
const endConfirmation = ref<{ rateId: string; effectiveUntil: string; label: string } | null>(null)
const catalog = ref<AsyncState<GpuCatalogEntrySchema[]>>({ kind: 'idle' })
const gpuOptions = computed(() => catalog.value.kind === 'success' ? catalog.value.data : [])
function gpuOptionKey(entry: GpuCatalogEntrySchema) { return `${entry.class}:${entry.mode}` }
const selectedGpu = computed(() => gpuOptions.value.find((entry) => gpuOptionKey(entry) === rateGpuSelection.value))
const gpuModeDescription = computed(() => selectedGpu.value ? ({
  exclusive: '独占：每个分配单位独占一张 GPU。',
  container_time_slice: '容器时间片：每个分配单位共享一张 GPU 的时间片。',
  vm_vgpu: 'VM vGPU：每个分配单位为目录指定的虚拟 GPU 规格。',
}[selectedGpu.value.mode]) : '请从目录选择实际分配类型。')
async function loadCatalog() {
  catalog.value = { kind: 'loading', message: '加载 GPU 类型…' }
  try {
    const result = await listResourceGpuCatalog()
    if (result.error) throw result.error
    const options = new Map<string, GpuCatalogEntrySchema>()
    for (const entry of result.data) {
      if (!entry.active) continue
      const key = gpuOptionKey(entry)
      if (!options.has(key) || options.get(key)!.revision < entry.revision) options.set(key, entry)
    }
    catalog.value = options.size ? { kind: 'success', data: [...options.values()] } : { kind: 'empty' }
  } catch (error) {
    const problem = extractProblemDetails(error)
    catalog.value = { kind: 'error', diagnostic: makeDiagnostic(problem?.diagnosticCode ?? 'GPU_CATALOG_LOAD_FAILED', problem?.detail ?? '无法加载 GPU 目录，不能创建 GPU 费率。', problem?.retryable ?? true) }
  }
}
const rateDateError = computed(() => {
  const from = Date.parse(rateEffectiveFrom.value)
  if (!Number.isFinite(from)) return '请输入有效生效时间。'
  if (from <= now.value) return '新版本须在未来生效，不能回溯修改历史计价。'
  if (rateEffectiveUntil.value && (!Number.isFinite(Date.parse(rateEffectiveUntil.value)) || Date.parse(rateEffectiveUntil.value) <= from)) return '结束时间须晚于生效时间。'
  return null
})

const selectedCharge = computed(() => finance.charges.kind === 'success'
  ? finance.charges.data.find((charge) => charge.id === selectedChargeId.value) ?? null
  : null)
const usageContextData = computed(() => finance.usageContext.kind === 'success' ? finance.usageContext.data : null)
const budgetEditable = computed(() => finance.budget.kind === 'success' || finance.budget.kind === 'empty')
const fixedDecimalPattern = /^(0|[1-9][0-9]*)\.[0-9]{6}$/
const fixedDecimalScale = 1_000_000n
function fixedDecimalScaled(value: string): bigint | null {
  if (!fixedDecimalPattern.test(value)) return null
  const [whole, fraction] = value.split('.')
  return BigInt(whole) * fixedDecimalScale + BigInt(fraction)
}
function compareFixedDecimalAmounts(left: string, right: string): number | null {
  const leftScaled = fixedDecimalScaled(left)
  const rightScaled = fixedDecimalScaled(right)
  if (leftScaled === null || rightScaled === null) return null
  return leftScaled < rightScaled ? -1 : leftScaled > rightScaled ? 1 : 0
}
const budgetNotice = computed(() => {
  if (finance.budget.kind !== 'success') return null
  const { spent, warningAt, limit } = finance.budget.data
  if (spent.currency !== warningAt.currency || spent.currency !== limit.currency) return null
  const spentVsWarning = compareFixedDecimalAmounts(spent.amount, warningAt.amount)
  const spentVsLimit = compareFixedDecimalAmounts(spent.amount, limit.amount)
  if (spentVsWarning === null || spentVsLimit === null) return null
  return {
    warningReached: spentVsWarning >= 0,
    limitReached: spentVsLimit >= 0,
  }
})
const canSaveBudget = computed(() => {
  if (!budgetEditable.value || !selectedProjectId.value) return false
  const warningVsLimit = compareFixedDecimalAmounts(warningAmount.value, limitAmount.value)
  return /^[A-Za-z0-9_-]{1,32}$/.test(budgetCurrency.value)
    && fixedDecimalScaled(limitAmount.value) !== null
    && fixedDecimalScaled(warningAmount.value) !== null
    && warningVsLimit !== null
    && warningVsLimit <= 0
})
const canSubmitAdjustment = computed(() => Boolean(selectedCharge.value && /^-?(0|[1-9][0-9]*)\.[0-9]{6}$/.test(adjustmentAmount.value) && adjustmentReason.value.trim()))
const canSubmitRate = computed(() => Boolean(
  rateUnitQuantity.value && canonicalRateAmount(rateAmount.value)
  && /^[A-Za-z0-9_-]{1,32}$/.test(rateCurrency.value)
  && (rateUnit.value !== 'gpu_unit_second' || selectedGpu.value)
  && !rateDateError.value,
))
const endingRate = computed(() => {
  if (!endingRateId.value || rates.rates.kind !== 'success') return null
  return rates.rates.data.find((rate) => rate.id === endingRateId.value) ?? null
})
const endDateError = computed(() => {
  if (!endingRate.value) return '请选择仍在使用中的费率版本。'
  if (endingRate.value.effectiveUntil) return '该费率已经安排结束，不能重复操作。'
  const until = Date.parse(endingEffectiveUntil.value)
  if (!Number.isFinite(until)) return '请输入有效截止时间。'
  if (until <= now.value) return '截止时间须在未来。'
  const from = Date.parse(endingRate.value.effectiveFrom)
  if (Number.isFinite(from) && until <= from) return '截止时间须晚于费率生效时间。'
  return null
})
const canSubmitEndRate = computed(() => Boolean(endingRate.value && !endDateError.value && rates.acting === null))
const endConfirmationDescription = computed(() => {
  const confirmation = endConfirmation.value
  if (!confirmation) return ''
  return `将把 ${confirmation.label} 安排在 ${formatTimestamp(confirmation.effectiveUntil)} 截止。截止后不再用于新的用量核算，历史账单快照会保留，现有环境不会被强制停止。请先核对在用资源、未结算用量或接替费率；此操作不可恢复，如需恢复只能创建新的费率版本。`
})

watch(selectedProjectId, () => {
  budgetCurrency.value = 'USD'
  limitAmount.value = '0.000000'
  warningAmount.value = '0.000000'
  selectedChargeId.value = ''
  adjustmentAmount.value = '0.000000'
  adjustmentReason.value = ''
})

watch(
  () => finance.budget,
  (state) => {
    if (state.kind === 'success') {
      budgetCurrency.value = state.data.limit.currency
      limitAmount.value = state.data.limit.amount
      warningAmount.value = state.data.warningAt.amount
    } else if (state.kind === 'empty') {
      budgetCurrency.value = 'USD'
      limitAmount.value = '0.000000'
      warningAmount.value = '0.000000'
    }
  },
)

function selectCharge(charge: ResourceCharge) {
  selectedChargeId.value = charge.id
  adjustmentAmount.value = '0.000000'
  adjustmentReason.value = ''
}

async function saveBudget() {
  const project = selectedProject.value
  if (!project || !canSaveBudget.value) return
  await finance.saveBudget({
    projectId: project.id,
    courseId: project.courseId,
    limit: { currency: budgetCurrency.value, amount: limitAmount.value },
    warningAt: { currency: budgetCurrency.value, amount: warningAmount.value },
  })
}

async function submitAdjustment() {
  const charge = selectedCharge.value
  if (!charge || !canSubmitAdjustment.value) return
  const ok = await finance.adjust(charge, {
    amount: { currency: charge.total.currency, amount: adjustmentAmount.value },
    reason: adjustmentReason.value.trim(),
  })
  if (ok) selectedChargeId.value = ''
}

function settlementLabel(value: ResourceCharge['settlement']) {
  return ({ pending: '待结算', settled: '已结算', unsettled: '未结算' } as Record<ResourceCharge['settlement'], string>)[value]
}

function usageKindLabel(value: ResourceUsageRecord['kind']) {
  return value === 'compute' ? '计算用量' : '存储用量'
}

function usageIntervalLabel(record: ResourceUsageRecord) {
  return `${formatTimestamp(record.measuredFrom)} 至 ${formatTimestamp(record.measuredUntil)}`
}

function usageEmptyText(page: ResourceUsagePageSchema) {
  if (page.page === 1 && !page.hasMore) return '该项目暂无用量记录。'
  return `本页没有项目用量记录。${page.hasMore ? '可查看下一页。' : ''}`
}

function environmentLabel(environmentId: string) {
  const environment = usageContextData.value?.environments.find((item) => item.id === environmentId)
  return environment?.displayLabel?.trim() || '环境名称待同步'
}

function requestStateLabel(value: string) {
  return ({
    reviewing: '待审核',
    allocating: '分配中',
    active: '已分配',
    expiring: '即将回收',
    expired: '已回收',
    rejected: '已拒绝',
    cancelled: '已取消',
  } as Record<string, string>)[value] ?? '状态待同步'
}

function usageTargetLabel(record: ResourceUsageRecord) {
  if (record.target.kind === 'experiment_environment') return `教学实验环境：${environmentLabel(record.target.environmentId)}`
  const requestId = usageRequestId(record)
  const request = requestId ? usageContextData.value?.requests.find((item) => item.id === requestId) : undefined
  if (!request) return '资源申请（名称待同步）'
  const state = requestStateLabel(request.state)
  if (request.target.kind === 'environment') return `资源环境：${environmentLabel(request.target.environmentId)} · ${state}`
  return `一次性任务资源 · ${state}`
}

function usageEnvironmentId(record: ResourceUsageRecord) {
  return record.target.kind === 'experiment_environment'
    ? record.target.environmentId
    : null
}

function usageRequestId(record: ResourceUsageRecord) {
  return record.target.kind === 'resource_request'
    ? record.target.requestId
    : null
}

function usageLeaseId(record: ResourceUsageRecord) {
  return record.target.kind === 'resource_request'
    ? record.target.leaseId ?? null
    : null
}

function usageQuantitiesLabel(quantities: ResourceUsageQuantities) {
  return [
    `CPU ${formatUsageNumber(quantities.cpuMillicoreSeconds)} millicore·秒`,
    `内存 ${formatUsageBytes(quantities.memoryByteSeconds)}`,
    `存储 ${formatUsageBytes(quantities.storageByteSeconds)}`,
    `GPU ${formatUsageNumber(quantities.gpuUnitSeconds)} 单位·秒`,
  ].join(' · ')
}

function formatUsageNumber(value: number) {
  return Math.trunc(value).toLocaleString('en-US')
}

function formatUsageBytes(value: number) {
  if (value === 0) return '0 B·秒'
  const units = ['B·秒', 'KiB·秒', 'MiB·秒', 'GiB·秒', 'TiB·秒']
  let amount = value
  let index = 0
  while (amount >= 1024 && index < units.length - 1) {
    amount /= 1024
    index += 1
  }
  const rounded = Math.round(amount * 1000) / 1000
  return `${rounded} ${units[index]}`
}

function chargeUsage(charge: ResourceCharge) {
  return finance.usageRecordsById[charge.usageRecordId] ?? null
}

function chargeTargetLabel(charge: ResourceCharge) {
  const usage = chargeUsage(charge)
  return usage ? usageTargetLabel(usage) : '项目用量关联待同步'
}

function billingUnitLabel(value: ResourceCharge['lines'][number]['unit']) {
  return ({
    cpu_millicore_second: 'CPU',
    memory_byte_second: '内存',
    storage_byte_second: '存储',
    gpu_unit_second: 'GPU',
  } as Record<ResourceCharge['lines'][number]['unit'], string>)[value] ?? value
}

function localDateTimeValue(date = new Date()) {
  const pad = (value: number) => String(value).padStart(2, '0')
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`
}

function toUtcTimestamp(value: string): string | null {
  const timestamp = Date.parse(value)
  return Number.isFinite(timestamp) ? new Date(timestamp).toISOString() : null
}

function rateLabel(rate: ResourceRateSchema) {
  if (rate.unit === 'gpu_unit_second') return `GPU ${rate.gpuClass} · ${rateModeLabel(rate.gpuMode)}`
  return billingUnitLabel(rate.unit)
}

function rateModeLabel(mode: GpuAllocationMode | null | undefined) {
  return ({ exclusive: '独占', container_time_slice: '容器时间片', vm_vgpu: 'VM vGPU' } as Record<GpuAllocationMode, string>)[mode ?? 'exclusive']
}

function beginEndRate(rate: ResourceRateSchema) {
  if (rate.effectiveUntil || rates.acting !== null) return
  endingRateId.value = rate.id
  endingEffectiveUntil.value = localDateTimeValue(new Date(Date.now() + 5 * 60_000))
  endConfirmation.value = null
}

function cancelEndRate() {
  endingRateId.value = null
  endingEffectiveUntil.value = ''
  endConfirmation.value = null
}

function openEndConfirmation(rate: ResourceRateSchema) {
  if (rate.id !== endingRateId.value || !canSubmitEndRate.value) return
  const effectiveUntil = toUtcTimestamp(endingEffectiveUntil.value)
  if (!effectiveUntil) return
  endConfirmation.value = { rateId: rate.id, effectiveUntil, label: rateLabel(rate) }
}

async function confirmEndRate() {
  const confirmation = endConfirmation.value
  if (!confirmation || rates.acting !== null) return
  const completed = await rates.end(confirmation.rateId, { effectiveUntil: confirmation.effectiveUntil })
  endConfirmation.value = null
  if (completed) cancelEndRate()
}

async function submitRate() {
  if (!canSubmitRate.value) return
  const effectiveFrom = toUtcTimestamp(rateEffectiveFrom.value)
  const effectiveUntil = rateEffectiveUntil.value ? toUtcTimestamp(rateEffectiveUntil.value) : null
  if (!effectiveFrom || (rateEffectiveUntil.value && !effectiveUntil)) return
  const created = await rates.create({
    unit: rateUnit.value,
    unitQuantity: rateUnitQuantity.value!,
    gpuClass: rateUnit.value === 'gpu_unit_second' ? selectedGpu.value!.class : null,
    gpuMode: rateUnit.value === 'gpu_unit_second' ? selectedGpu.value!.mode : null,
    unitPrice: { currency: rateCurrency.value.trim(), amount: canonicalRateAmount(rateAmount.value)! },
    effectiveFrom,
    effectiveUntil,
  })
  if (!created) return
  rateAmount.value = ''
  rateEffectiveFrom.value = localDateTimeValue(new Date(Date.now() + 5 * 60_000))
  rateEffectiveUntil.value = ''
}

onMounted(() => { void rates.load(); void loadCatalog() })
</script>

<style scoped>
.finance-page { display: grid; gap: 20px; }
.page-header, .section-heading { display: flex; align-items: flex-start; justify-content: space-between; gap: 16px; }
.page-header h2, .section-heading h3, .section-heading h4 { margin: 0; color: var(--md-sys-color-on-surface); }
.page-header h2 { font: var(--md-sys-headline-small); }
.section-heading h3 { font: var(--md-sys-title-large); }
.section-heading h4 { font: var(--md-sys-title-medium); }
.page-subtitle, .section-heading p, .section-heading small, .updated-note { margin: 6px 0 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); line-height: 1.5; }
.project-strip { display: flex; align-items: end; gap: 16px; padding: 16px 20px; }
.project-strip label, .budget-form label, .adjustment-form label { display: grid; gap: 6px; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.project-strip label { flex: 1; max-width: 560px; }
.project-scope { padding-bottom: 10px; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.project-context-action { justify-self: start; }
.rates-card { display: grid; gap: 18px; padding: 20px; }
.rate-list { display: grid; gap: 8px; margin: 0; padding: 0; list-style: none; }
.rate-row { display: grid; gap: 10px; padding: 14px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low); }
.rate-row-main { display: flex; align-items: center; justify-content: space-between; gap: 14px; }
.rate-main { display: grid; gap: 4px; min-width: 0; }
.rate-main strong { color: var(--md-sys-color-on-surface); font: var(--md-sys-title-medium); }
.rate-main small { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); overflow-wrap: anywhere; }
.rate-actions { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; justify-content: flex-end; }
.rate-end-editor { display: grid; gap: 10px; padding-top: 12px; border-top: 1px solid var(--md-sys-color-outline-variant); }
.rate-end-editor label { display: grid; gap: 6px; max-width: 320px; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.rate-end-actions { display: flex; align-items: center; gap: 8px; }
.rate-form { display: grid; gap: 12px; grid-template-columns: repeat(auto-fit, minmax(min(210px, 100%), 1fr)); align-items: end; }
.rate-form label { display: grid; gap: 6px; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.rate-form button { justify-self: start; }
.rate-equivalent { grid-column: 1 / -1; margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); line-height: 1.5; }
.finance-layout { display: grid; grid-template-columns: minmax(300px, .75fr) minmax(0, 1.25fr); gap: 20px; align-items: start; }
.usage-card { display: grid; grid-column: 1 / -1; gap: 18px; padding: 20px; }
.usage-list { display: grid; gap: 8px; margin: 0; padding: 0; list-style: none; }
.usage-row { display: flex; align-items: flex-start; justify-content: space-between; gap: 14px; padding: 14px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low); }
.usage-main { display: grid; gap: 5px; min-width: 0; }
.usage-main strong { color: var(--md-sys-color-on-surface); font: var(--md-sys-title-medium); }
.usage-main small, .usage-quantities { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); overflow-wrap: anywhere; }
.usage-quantities { color: var(--md-sys-color-on-surface); }
.usage-empty { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); }
.usage-pagination { display: flex; align-items: center; justify-content: flex-end; gap: 10px; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.advanced-details { display: grid; gap: 4px; margin-top: 2px; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); }
.advanced-details summary { color: var(--md-sys-color-primary); cursor: pointer; }
.budget-card, .charges-card { display: grid; gap: 18px; padding: 20px; }
.budget-summary { display: grid; gap: 10px; grid-template-columns: repeat(auto-fit, minmax(180px, 1fr)); }
.budget-summary div { display: grid; gap: 5px; padding: 13px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low); }
.budget-summary span { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); }
.budget-summary strong { color: var(--md-sys-color-on-surface); font: var(--md-sys-title-medium); white-space: nowrap; }
.budget-form { display: grid; gap: 12px; }
.text-input { box-sizing: border-box; min-height: 40px; width: 100%; padding: 8px 11px; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface); color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); }
textarea.text-input { resize: vertical; }
.charge-list { display: grid; gap: 8px; margin: 0; padding: 0; list-style: none; }
.charge-row { display: flex; align-items: center; justify-content: space-between; gap: 14px; padding: 14px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low); }
.charge-main { display: grid; gap: 4px; min-width: 0; }
.charge-main strong { color: var(--md-sys-color-on-surface); font: var(--md-sys-title-medium); }
.charge-main small { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); overflow-wrap: anywhere; }
.charge-line-list { display: grid; gap: 5px; margin: 7px 0 0; padding: 7px 0 0; border-top: 1px solid var(--md-sys-color-outline-variant); list-style: none; }
.charge-line { display: grid; grid-template-columns: minmax(70px, auto) minmax(0, 1fr) auto; gap: 8px; align-items: baseline; font: var(--md-sys-label-small); }
.charge-line > span { color: var(--md-sys-color-on-surface); }
.charge-line > small { overflow-wrap: anywhere; }
.charge-line > strong { color: var(--md-sys-color-on-surface); font: var(--md-sys-label-small); white-space: nowrap; }
.charge-actions { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; justify-content: flex-end; }
.state-chip { display: inline-flex; white-space: nowrap; padding: 3px 8px; border-radius: var(--md-sys-shape-full); background: var(--md-sys-color-surface-variant); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); }
.state-chip--settled { background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); }
.state-chip--pending, .state-chip--unsettled { background: var(--md-sys-color-tertiary-container); color: var(--md-sys-color-on-tertiary-container); }
.warning-text { color: var(--md-sys-color-error) !important; }
.adjustment-form { display: grid; gap: 12px; padding-top: 18px; border-top: 1px solid var(--md-sys-color-outline-variant); }
.section-heading--compact { align-items: center; }
.two-columns { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 10px; }
.filled-button, .outlined-button, .text-button { display: inline-flex; align-items: center; justify-content: center; gap: 7px; min-height: 40px; padding: 0 17px; border-radius: var(--md-sys-shape-full); font: var(--md-sys-label-large); cursor: pointer; text-decoration: none; }
.filled-button { border: 1px solid var(--md-sys-color-primary); background: var(--md-sys-color-primary); color: var(--md-sys-color-on-primary); }
.outlined-button { border: 1px solid var(--md-sys-color-outline); background: transparent; color: var(--md-sys-color-primary); }
.outlined-button.small { min-height: 32px; padding: 0 11px; font: var(--md-sys-label-medium); }
.text-button { min-height: 32px; padding: 0 8px; border: 0; background: transparent; color: var(--md-sys-color-primary); }
.filled-button:disabled, .outlined-button:disabled, .text-button:disabled { opacity: .5; cursor: not-allowed; }
.icon-button { display: inline-grid; place-items: center; width: 40px; height: 40px; border: 0; border-radius: 50%; background: transparent; color: var(--md-sys-color-on-surface-variant); cursor: pointer; }
@media (max-width: 850px) { .finance-layout { grid-template-columns: 1fr; } .project-strip { align-items: stretch; flex-direction: column; } .project-strip label { max-width: none; } .project-scope { padding-bottom: 0; } }
@media (max-width: 620px) { .budget-summary, .two-columns { grid-template-columns: 1fr; } .rate-row-main, .charge-row { align-items: flex-start; flex-direction: column; } .rate-actions, .charge-actions { justify-content: flex-start; } .charge-line { grid-template-columns: 1fr auto; } .charge-line > small { grid-column: 1 / -1; } }
</style>
