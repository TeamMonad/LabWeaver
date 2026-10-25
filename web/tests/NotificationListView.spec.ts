import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createMemoryHistory, createRouter } from 'vue-router'
import NotificationListView from '@/views/NotificationListView.vue'
import { getNotificationUnreadCount, listNotifications, markNotificationRead } from '@/generated/contracts'
import type { Notification, NotificationEmailState } from '@/generated/contracts'
import { notificationEmailStatus, notificationObjectLocation, unreadBadgeText } from '@/utils/notifications'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    getNotificationUnreadCount: vi.fn(),
    listNotifications: vi.fn(),
    markNotificationRead: vi.fn(),
  }
})

const OBJECT_PATH = '/researcher/resources?projectId=project-1&requestId=request-1'

function notification(id: string, extra: Partial<Notification> = {}): Notification {
  return {
    id,
    eventId: `event-${id}`,
    projectId: 'project-1',
    kind: 'resource_decision',
    objectUrl: OBJECT_PATH,
    observedAt: '2026-10-20T08:00:00.000Z',
    summary: `资源申请已批准 ${id}。`,
    emailState: 'disabled',
    emailAttempts: 0,
    ...extra,
  }
}

async function mountView(items: Notification[], nextCursor: string | null = null) {
  vi.mocked(listNotifications).mockResolvedValueOnce({ data: { items, nextCursor }, error: undefined } as never)
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/notifications', component: NotificationListView },
      { path: '/researcher/resources', component: { template: '<div data-testid="object-page" />' } },
    ],
  })
  await router.push('/notifications')
  await router.isReady()
  const wrapper = mount(NotificationListView, { global: { plugins: [router] } })
  await flushPromises()
  return { wrapper, router }
}

beforeEach(() => {
  vi.mocked(listNotifications).mockReset()
  vi.mocked(markNotificationRead).mockReset()
  vi.mocked(getNotificationUnreadCount).mockReset().mockResolvedValue({ data: { unreadCount: 0 }, error: undefined } as never)
})

describe('NotificationListView', () => {
  it('marks the notification read and then opens its object page', async () => {
    vi.mocked(markNotificationRead).mockResolvedValueOnce({ data: notification('n1', { readAt: '2026-10-20T08:05:00.000Z' }), error: undefined } as never)
    const { wrapper, router } = await mountView([notification('n1')])
    expect(wrapper.get('[data-testid="notification-item"]').text()).toContain('未读')

    await wrapper.get('.notification-row__open').trigger('click')
    await flushPromises()

    expect(markNotificationRead).toHaveBeenCalledTimes(1)
    expect(router.currentRoute.value.fullPath).toBe(OBJECT_PATH)
    wrapper.unmount()
  })

  it('opens the object even when marking read fails, leaving it unread', async () => {
    vi.mocked(markNotificationRead).mockResolvedValueOnce({
      data: undefined,
      error: { type: 'about:blank', title: 'error', status: 503, detail: 'down', diagnosticCode: 'LW_NOTIFICATION_PERSISTENCE_FAILED', requestId: 'req-503', retryable: true },
    } as never)
    const { wrapper, router } = await mountView([notification('n1')])

    await wrapper.get('.notification-row__open').trigger('click')
    await flushPromises()

    expect(router.currentRoute.value.fullPath).toBe(OBJECT_PATH)
    expect(markNotificationRead).toHaveBeenCalledTimes(1)
    wrapper.unmount()
  })

  it('explains withdrawn access in Chinese before the code and request id, without opening the object', async () => {
    vi.mocked(markNotificationRead).mockResolvedValueOnce({
      data: undefined,
      error: { type: 'about:blank', title: 'Not Found', status: 404, detail: 'notification not found', diagnosticCode: 'LW_NOTIFICATION_NOT_FOUND', requestId: 'req-404', retryable: false },
    } as never)
    const { wrapper, router } = await mountView([notification('n1', { summary: '受限项目的申请摘要' }), notification('n2')])

    await wrapper.get('.notification-row__open').trigger('click')
    await flushPromises()

    expect(router.currentRoute.value.path).toBe('/notifications')
    const banner = wrapper.get('[data-testid="notification-action-failure"]')
    const text = banner.text()
    expect(text).toContain('这条通知已不可查看')
    expect(text.indexOf('这条通知已不可查看')).toBeLessThan(text.indexOf('LW_NOTIFICATION_NOT_FOUND'))
    expect(text).toContain('req-404')
    expect(banner.find('.diagnostic-retry').exists()).toBe(false)
    // The withdrawn row and its summary are gone; the other notification stays.
    expect(wrapper.text()).not.toContain('受限项目的申请摘要')
    expect(wrapper.findAll('[data-testid="notification-item"]')).toHaveLength(1)
    wrapper.unmount()
  })

  it('shows the unknown delivery detail and never claims the email was sent', async () => {
    const { wrapper } = await mountView([
      notification('n1', {
        emailState: 'delivery_unknown',
        emailAttempts: 1,
        emailDetail: '邮件可能已送达，系统无法确认；为避免重复发送，不会自动重发。',
        emailDiagnostic: 'LW_NOTIFICATION_SMTP_DELIVERY_UNKNOWN',
      }),
    ])
    const status = wrapper.get('[data-testid="notification-email-status"]')
    expect(status.text()).toContain('邮件投递结果未知')
    expect(status.text()).toContain('邮件可能已送达，系统无法确认；为避免重复发送，不会自动重发。')
    expect(status.text()).toContain('LW_NOTIFICATION_SMTP_DELIVERY_UNKNOWN')
    expect(status.text()).not.toContain('已发送')
    wrapper.unmount()
  })

  it('loads older notifications by cursor and says when the list is complete', async () => {
    const { wrapper } = await mountView([notification('n1')], 'cursor-1')
    vi.mocked(listNotifications).mockResolvedValueOnce({ data: { items: [notification('n2')], nextCursor: null }, error: undefined } as never)

    await wrapper.get('button.outlined-button').trigger('click')
    await flushPromises()

    expect(listNotifications).toHaveBeenLastCalledWith({ query: { cursor: 'cursor-1', limit: 20 } })
    expect(wrapper.findAll('[data-testid="notification-item"]')).toHaveLength(2)
    expect(wrapper.find('button.outlined-button').exists()).toBe(false)
    expect(wrapper.text()).toContain('已显示全部通知。')
    wrapper.unmount()
  })
})

describe('notificationEmailStatus', () => {
  const cases: Array<[NotificationEmailState, string]> = [
    ['disabled', '仅站内通知'],
    ['pending', '邮件待发送'],
    ['sent', '邮件已发送'],
    ['failed', '邮件发送失败'],
    ['exhausted', '邮件未能发送'],
    ['no_verified_email', '未发送邮件'],
    ['delivery_unknown', '邮件投递结果未知'],
  ]

  it.each(cases)('labels %s as %s', (emailState, label) => {
    const status = notificationEmailStatus({ emailState, emailAttempts: 5, emailDetail: null, emailDiagnostic: 'LW_NOTIFICATION_SMTP_FAILED' })
    expect(status.label).toBe(label)
    expect(status.detail).not.toBe('')
    if (emailState !== 'sent') expect(`${status.label}${status.detail}`).not.toContain('已发送')
  })

  it('keeps the server diagnostic only for failed and unknown deliveries', () => {
    const withCode = (emailState: NotificationEmailState) =>
      notificationEmailStatus({ emailState, emailAttempts: 5, emailDiagnostic: 'LW_NOTIFICATION_SMTP_FAILED' }).diagnosticCode
    expect(withCode('failed')).toBe('LW_NOTIFICATION_SMTP_FAILED')
    expect(withCode('exhausted')).toBe('LW_NOTIFICATION_SMTP_FAILED')
    expect(withCode('delivery_unknown')).toBe('LW_NOTIFICATION_SMTP_FAILED')
    expect(withCode('sent')).toBeNull()
    expect(withCode('disabled')).toBeNull()
  })

  it('reports the attempt count once retries are exhausted', () => {
    expect(notificationEmailStatus({ emailState: 'exhausted', emailAttempts: 5 }).detail).toContain('已尝试 5 次')
  })

  it('uses the server detail for an unknown delivery and falls back when it is absent', () => {
    expect(notificationEmailStatus({ emailState: 'delivery_unknown', emailAttempts: 1, emailDetail: '服务器说明' }).detail).toContain('服务器说明')
    expect(notificationEmailStatus({ emailState: 'delivery_unknown', emailAttempts: 1 }).detail).toContain('系统无法确认邮件是否送达')
  })
})

describe('notificationObjectLocation', () => {
  it('follows same-origin paths only', () => {
    expect(notificationObjectLocation(OBJECT_PATH)).toBe(OBJECT_PATH)
    expect(notificationObjectLocation('https://example.invalid/x')).toBeNull()
    expect(notificationObjectLocation('//example.invalid/x')).toBeNull()
    expect(notificationObjectLocation('/\\example.invalid')).toBeNull()
    expect(notificationObjectLocation('javascript:alert(1)')).toBeNull()
  })
})

describe('unreadBadgeText', () => {
  it('caps large counts', () => {
    expect(unreadBadgeText(7)).toBe('7')
    expect(unreadBadgeText(100)).toBe('99+')
  })
})
