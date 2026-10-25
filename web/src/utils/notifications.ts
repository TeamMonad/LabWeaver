import type { Notification, NotificationEmailState, NotificationKind } from '@/generated/contracts'

export function notificationKindLabel(kind: NotificationKind): string {
  switch (kind) {
    case 'resource_decision':
      return '资源申请结果'
    case 'lease_changed':
      return '资源授权变更'
    case 'lease_expiry_reminder':
      return '到期提醒'
    case 'short_lease':
      return '短租期提示'
  }
}

export interface NotificationEmailStatus {
  label: string
  /** Chinese reason and next step; shown before any diagnostic code. */
  detail: string
  tone: 'neutral' | 'success' | 'warning' | 'error'
  /** Server diagnostic for a failed or unknown delivery, kept for support. */
  diagnosticCode: string | null
}

type EmailFacts = Pick<Notification, 'emailState' | 'emailDetail' | 'emailDiagnostic' | 'emailAttempts'>

const UNKNOWN_DELIVERY_FALLBACK = '系统无法确认邮件是否送达；为避免重复发送，不会自动重发。'

/**
 * What the platform knows about the optional email copy. The in-app
 * notification is complete in every state; only `sent` claims a delivery.
 */
export function notificationEmailStatus(facts: EmailFacts): NotificationEmailStatus {
  const state: NotificationEmailState = facts.emailState
  const diagnosticCode = facts.emailDiagnostic?.trim() || null
  switch (state) {
    case 'disabled':
      return { label: '仅站内通知', detail: '平台未启用邮件通知，站内通知照常保存。', tone: 'neutral', diagnosticCode: null }
    case 'pending':
      return { label: '邮件待发送', detail: '邮件正在排队发送；站内通知已保存。', tone: 'neutral', diagnosticCode: null }
    case 'sent':
      return { label: '邮件已发送', detail: '邮件服务器已接收这封邮件。', tone: 'success', diagnosticCode: null }
    case 'failed':
      // The same state covers a temporary rejection that is still retried and a final one.
      return {
        label: '邮件发送失败',
        detail: '邮件服务器拒收了这封邮件。临时拒收时平台会在 5 次以内自动重试，永久拒收不再重试。站内通知不受影响。',
        tone: 'error',
        diagnosticCode,
      }
    case 'exhausted':
      return {
        label: '邮件未能发送',
        detail: `邮件服务器多次临时拒收（已尝试 ${facts.emailAttempts} 次），平台已停止重试。站内通知不受影响。`,
        tone: 'error',
        diagnosticCode,
      }
    case 'no_verified_email':
      return {
        label: '未发送邮件',
        detail: '你的组织账号没有已验证的邮箱地址，因此不发送邮件。站内通知不受影响；如需邮件提醒，请在组织登录系统中验证邮箱后重新登录。',
        tone: 'neutral',
        diagnosticCode: null,
      }
    case 'delivery_unknown':
      return {
        label: '邮件投递结果未知',
        detail: `${facts.emailDetail?.trim() || UNKNOWN_DELIVERY_FALLBACK} 请以站内通知为准，不需要重复操作资源。`,
        tone: 'warning',
        diagnosticCode,
      }
  }
}

const LINK_BASE = 'https://labweaver.invalid'

/**
 * The in-app location a notification links to. Only same-origin absolute
 * paths are followed; anything else is refused instead of navigating away.
 * Following the link grants nothing: the target page authorizes as usual.
 */
export function notificationObjectLocation(objectUrl: string): string | null {
  const value = objectUrl.trim()
  if (!value.startsWith('/') || value.startsWith('//') || value.includes('\\')) return null
  let parsed: URL
  try {
    parsed = new URL(value, LINK_BASE)
  } catch {
    return null
  }
  if (parsed.origin !== LINK_BASE) return null
  return `${parsed.pathname}${parsed.search}${parsed.hash}`
}

/** Badge text for the unread count; large counts are capped so the badge keeps its size. */
export function unreadBadgeText(count: number): string {
  return count > 99 ? '99+' : String(count)
}
