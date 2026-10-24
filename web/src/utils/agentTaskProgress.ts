import type { AgentRunTaskProgress, AgentTrackKind, AgentTrackTaskProgress, TaskAttemptPhase } from '@/generated/contracts'
import type { StatusPillTone } from '@/types/statusPill'

export interface AgentTrackReading {
  track: AgentTrackKind
  trackLabel: string
  phase: TaskAttemptPhase
  phaseLabel: string
  /** One Chinese sentence explaining the phase, used as the pill tooltip. */
  phaseDetail: string
  tone: StatusPillTone
  /** Native attempt number; null before the owner claims the first attempt (evaluation reports 0). */
  attempt: number | null
  /** Chinese sentence telling a person what has to happen next; null when nobody needs to act. */
  attention: string | null
}

const TRACK_LABEL: Record<AgentTrackKind, string> = {
  environment: '环境',
  evaluation: '评测',
  work_configuration: 'Work 配置',
}

const PHASE: Record<TaskAttemptPhase, { label: string; detail: string; tone: StatusPillTone }> = {
  queued: { label: '排队中', detail: '这次尝试已受理，正在排队等待执行。', tone: 'neutral' },
  preparing: { label: '准备中', detail: '执行方已接手，正在准备这次尝试。', tone: 'running' },
  waiting_resource_approval: { label: '等待资源审批', detail: '这次尝试需要的资源正在等待审批，批准后自动继续。', tone: 'warning' },
  allocating: { label: '分配资源中', detail: '资源已批准，正在分配。', tone: 'running' },
  executing: { label: '执行中', detail: '这次尝试正在执行。', tone: 'running' },
  awaiting_review: { label: '等待人工审核', detail: '这次尝试已执行完，等待人工审核后才会继续。', tone: 'warning' },
  cleanup_pending: { label: '清理中', detail: '正在清理这次尝试使用的资源。', tone: 'running' },
  succeeded: { label: '已完成', detail: '这次尝试已完成。', tone: 'success' },
  failed: { label: '失败', detail: '这次尝试失败，请打开任务查看原因。', tone: 'error' },
  cancelled: { label: '已取消', detail: '这次尝试已取消。', tone: 'neutral' },
  blocked: { label: '受阻', detail: '这次尝试无法继续，请打开任务查看原因。', tone: 'error' },
}

const TRACK_ORDER: AgentTrackKind[] = ['work_configuration', 'environment', 'evaluation']

function attentionFor(trackLabel: string, phase: TaskAttemptPhase): string | null {
  if (phase === 'waiting_resource_approval') return `${trackLabel}正在等待资源审批，批准后自动继续。`
  if (phase === 'awaiting_review') return `${trackLabel}已执行完，等待人工审核后才会继续。`
  return null
}

export function readAgentTrack(item: AgentTrackTaskProgress): AgentTrackReading {
  const phase = PHASE[item.progress.phase]
  const trackLabel = TRACK_LABEL[item.track]
  return {
    track: item.track,
    trackLabel,
    phase: item.progress.phase,
    phaseLabel: phase.label,
    phaseDetail: phase.detail,
    tone: phase.tone,
    attempt: item.progress.attempt >= 1 ? item.progress.attempt : null,
    attention: attentionFor(trackLabel, item.progress.phase),
  }
}

/**
 * Current attempt of every track, in a stable order. History is never mixed in,
 * so an earlier failed attempt cannot hide the attempt that is running now.
 */
export function readAgentTaskProgress(progress: AgentRunTaskProgress): AgentTrackReading[] {
  return [...progress.current]
    .sort((a, b) => TRACK_ORDER.indexOf(a.track) - TRACK_ORDER.indexOf(b.track))
    .map(readAgentTrack)
}
