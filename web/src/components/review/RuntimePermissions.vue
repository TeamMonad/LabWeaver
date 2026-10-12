<template>
  <section class="runtime-permissions" :aria-labelledby="headingId" data-testid="runtime-permissions">
    <h4 :id="headingId">{{ title }}</h4>

    <div v-for="group in groups" :key="group.label" class="perm-group">
      <h5>{{ group.label }}</h5>
      <dl>
        <template v-for="(row, index) in group.rows" :key="`${row.label}-${index}`">
          <dt>{{ row.label }}</dt>
          <dd :data-tone="row.tone">
            <SvgIcon :name="row.tone === 'restricted' ? 'lock' : row.tone === 'open' ? 'lock_open' : 'info'" size="sm" aria-hidden="true" />
            <span>{{ row.value }}<small v-if="row.hint">{{ row.hint }}</small></span>
          </dd>
        </template>
      </dl>
    </div>

    <template v-if="receiptGroups.length">
      <div
        v-for="group in receiptGroups"
        :key="group.label"
        class="perm-group perm-group--receipt"
        data-testid="runtime-authorization-receipt"
      >
        <h5>{{ group.label }}</h5>
        <dl>
          <template v-for="(row, index) in group.rows" :key="`${row.label}-${index}`">
            <dt>{{ row.label }}</dt>
            <dd :data-tone="row.tone">
              <SvgIcon name="info" size="sm" aria-hidden="true" />
              <span>{{ row.value }}<small v-if="row.hint">{{ row.hint }}</small></span>
            </dd>
          </template>
        </dl>
      </div>
    </template>

    <div v-if="missing.length" class="perm-group perm-group--missing" data-testid="permission-gap">
      <h5>仍待平台提供</h5>
      <p>下列对照信息候选规格本身不携带，这里不做推断：</p>
      <ul>
        <li v-for="item in missing" :key="item">{{ item }}</li>
      </ul>
    </div>

    <p class="perm-note">
      运行权限随环境配置、镜像和评分规则一起绑定到批准的版本。构建成功不代表已获得运行权限；创建、启动和重启时，平台按原 ResourceApproval 回执与管理员策略检查。UID / root 与 privileged 相互独立。
    </p>
  </section>
</template>

<script setup lang="ts">
import { computed, useId } from 'vue'
import SvgIcon from '@/components/common/SvgIcon.vue'
import type {
  EnvironmentCandidateViewSchema,
  EnvironmentRuntimeAuthorizationResolution,
} from '@/generated/contracts'
import {
  MISSING_PERMISSION_FIELDS,
  runtimeAuthorizationReceiptGroups,
  runtimePermissionGroups,
} from '@/utils/runtimePermissions'

const props = withDefaults(defineProps<{
  spec: EnvironmentCandidateViewSchema['candidate']['spec']
  runtimeAuthorization?: EnvironmentRuntimeAuthorizationResolution | null
  title?: string
}>(), { title: '运行权限', runtimeAuthorization: null })

const headingId = `runtime-permissions-${useId()}`
const groups = computed(() => runtimePermissionGroups(props.spec))
const receiptGroups = computed(() =>
  props.runtimeAuthorization ? runtimeAuthorizationReceiptGroups(props.runtimeAuthorization) : [],
)
const missing = MISSING_PERMISSION_FIELDS
</script>

<style scoped>
.runtime-permissions { display: grid; gap: 12px; }
.runtime-permissions h4 { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-medium); }
.perm-group { display: grid; gap: 6px; }
.perm-group h5 { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-large); }
.perm-group dl { display: grid; grid-template-columns: minmax(96px, 132px) minmax(0, 1fr); gap: 6px 16px; margin: 0; }
.perm-group dt { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); }
.perm-group dd { display: flex; gap: 6px; align-items: flex-start; margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); overflow-wrap: anywhere; }
.perm-group dd small { display: block; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.perm-group dd[data-tone='restricted'] :deep(.svg-icon) { color: var(--md-sys-color-primary); }
.perm-group dd[data-tone='open'] :deep(.svg-icon) { color: var(--md-sys-color-tertiary); }
.perm-group--receipt { padding: 10px 12px; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low); }
.perm-group--missing { padding: 10px 12px; border: 1px dashed var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-small); }
.perm-group--missing p, .perm-group--missing ul { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.perm-group--missing ul { padding-left: 18px; }
.perm-note { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
@media (max-width: 720px) { .perm-group dl { grid-template-columns: 1fr; gap: 2px; } }
</style>
