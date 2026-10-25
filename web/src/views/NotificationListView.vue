<template>
  <div class="notification-page">
    <header class="page-header">
      <div>
        <h2>通知</h2>
        <p class="page-subtitle">资源申请结果、授权变更和到期提醒。打开通知会把它标记为已读；对象页面仍按你当前的权限显示内容。</p>
      </div>
      <button type="button" class="icon-button" aria-label="刷新通知" :disabled="list.state.kind === 'loading'" @click="reload">
        <SvgIcon name="refresh" size="sm" aria-hidden="true" />
      </button>
    </header>

    <DiagnosticBanner
      v-if="list.actionFailure"
      :diagnostic="list.actionFailure.diagnostic"
      :severity="list.actionFailure.diagnostic.retryable ? 'error' : 'warning'"
      data-testid="notification-action-failure"
    />

    <section class="md-card notification-card" aria-labelledby="notification-list-heading">
      <h3 id="notification-list-heading" class="visually-hidden">通知列表</h3>
      <AsyncStateView :state="list.state" empty-text="暂无通知。资源申请有结果或授权即将到期时会在这里出现。" @retry="list.load">
        <template #success="{ data }">
          <ul class="notification-list">
            <li
              v-for="notification in data"
              :key="notification.id"
              class="notification-row"
              :class="{ 'notification-row--unread': !notification.readAt }"
              data-testid="notification-item"
              :data-notification-id="notification.id"
            >
              <button
                type="button"
                class="notification-row__open"
                :disabled="list.opening !== null"
                :aria-busy="list.opening === notification.id"
                @click="openNotification(notification)"
              >
                <span class="notification-row__heading">
                  <span v-if="!notification.readAt" class="unread-dot" aria-hidden="true" />
                  <strong>{{ notificationKindLabel(notification.kind) }}</strong>
                  <span class="notification-row__read-state">{{ notification.readAt ? '已读' : '未读' }}</span>
                </span>
                <span class="notification-row__summary">{{ notification.summary }}</span>
                <small>
                  {{ formatTimestamp(notification.observedAt) }}
                  <template v-if="notification.deadline"> · 期限 {{ formatTimestamp(notification.deadline) }}</template>
                </small>
              </button>
              <div
                class="email-status"
                :class="`email-status--${emailStatus(notification).tone}`"
                data-testid="notification-email-status"
              >
                <span class="email-status__label">{{ emailStatus(notification).label }}</span>
                <span class="email-status__detail">{{ emailStatus(notification).detail }}</span>
                <details v-if="emailStatus(notification).diagnosticCode" class="advanced-details">
                  <summary>查看诊断信息</summary>
                  <small>诊断码：{{ emailStatus(notification).diagnosticCode }}</small>
                </details>
              </div>
            </li>
          </ul>
          <div class="notification-more">
            <DiagnosticBanner v-if="list.moreError" :diagnostic="list.moreError" @retry="list.loadMore" />
            <button
              v-if="list.nextCursor"
              type="button"
              class="outlined-button"
              :disabled="list.loadingMore"
              @click="list.loadMore"
            >
              {{ list.loadingMore ? '正在加载…' : '加载更早的通知' }}
            </button>
            <small v-else class="notification-end">已显示全部通知。</small>
          </div>
        </template>
      </AsyncStateView>
    </section>
  </div>
</template>

<script setup lang="ts">
import { onMounted } from 'vue'
import { useRouter } from 'vue-router'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import SvgIcon from '@/components/common/SvgIcon.vue'
import { refreshUnreadCount, useNotificationList } from '@/composables/useNotifications'
import type { Notification } from '@/generated/contracts'
import { formatTimestamp } from '@/utils/format'
import { notificationEmailStatus as emailStatus, notificationKindLabel } from '@/utils/notifications'

const router = useRouter()
const list = useNotificationList()

function reload() {
  void list.load()
  void refreshUnreadCount()
}

async function openNotification(notification: Notification) {
  const location = await list.open(notification)
  if (location) await router.push(location)
}

onMounted(() => { void list.load() })
</script>

<style scoped>
.notification-page { display: grid; gap: var(--space-5); }
.page-header { display: flex; align-items: flex-start; justify-content: space-between; gap: var(--space-4); }
.page-header h2 { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-headline-small); }
.page-subtitle { margin: var(--space-2) 0 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); line-height: 1.5; }
.notification-card { padding: var(--space-5); }
.visually-hidden { position: absolute; width: 1px; height: 1px; overflow: hidden; clip: rect(0 0 0 0); white-space: nowrap; }
.notification-list { display: grid; gap: var(--space-2); margin: 0; padding: 0; list-style: none; }
.notification-row { display: grid; grid-template-columns: minmax(0, 1fr) minmax(220px, 0.6fr); gap: var(--space-4); padding: 13px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low); }
.notification-row--unread { background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); }
.notification-row__open { display: grid; gap: var(--space-1); min-width: 0; padding: 0; border: 0; background: none; color: inherit; font: inherit; text-align: left; cursor: pointer; }
.notification-row__open:disabled { cursor: progress; }
.notification-row__open:focus-visible { outline: 2px solid var(--md-sys-color-primary); outline-offset: 4px; border-radius: var(--md-sys-shape-small); }
.notification-row__heading { display: flex; align-items: center; gap: var(--space-2); font: var(--md-sys-title-small); }
.notification-row__read-state { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); }
.notification-row__summary { font: var(--md-sys-body-medium); overflow-wrap: anywhere; }
.notification-row__open small { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); }
.unread-dot { width: 8px; height: 8px; border-radius: 50%; background: var(--md-sys-color-primary); }
.email-status { display: grid; gap: var(--space-1); align-content: start; font: var(--md-sys-body-small); color: var(--md-sys-color-on-surface-variant); }
.email-status__label { font: var(--md-sys-label-large); color: var(--md-sys-color-on-surface); }
.email-status--success .email-status__label { color: var(--md-sys-color-primary); }
.email-status--warning .email-status__label { color: var(--md-sys-color-tertiary); }
.email-status--error .email-status__label { color: var(--md-sys-color-error); }
.advanced-details { font: var(--md-sys-label-small); }
.advanced-details summary { cursor: pointer; }
.notification-more { display: grid; justify-items: center; gap: var(--space-3); margin-top: var(--space-4); }
.notification-end { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); }
@media (max-width: 720px) { .notification-row { grid-template-columns: 1fr; } }
</style>
