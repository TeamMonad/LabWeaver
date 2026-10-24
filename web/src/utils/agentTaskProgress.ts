import type { AgentRunTaskProgress, AgentTrackKind, AgentTrackTaskProgress, TaskAttemptPhase } from '@/generated/contracts'

/** Visual tone of one track's current attempt, mapped onto the shared status pill colours. */
export type TrackTone = 'neutral' | 'progress' | 'attention' | 'success' | 'error'

export interface AgentTrackReading {
  track: AgentTrackKind
  trackLabel: string
  phase: TaskAttemptPhase
  phaseLabel: string
  tone: TrackTone
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

const PHASE: Record<TaskAttemptPhase, { label: string; tone: TrackTone }> = {
  queued: { label: '排队中', tone: 'neutral' },
  preparing: { label: '准备中', tone: 'progress' },
  waiting_resource_approval: { label: '等待资源审批', tone: 'attention' },
  allocating: { label: '分配资源中', tone: 'progress' },
  executing: { label: '执行中', tone: 'progress' },
  awaiting_review: { label: '等待人工审核', tone: 'attention' },
  cleanup_pending: { label: '清理中', tone: 'progress' },
  succeeded: { label: '已完成', tone: 'success' },
  failed: { label: '失败', tone: 'error' },
  cancelled: { label: '已取消', tone: 'neutral' },
  blocked: { label: '受阻', tone: 'error' },
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

/** A state string the shared status pill already colours as this tone. */
export function toneState(tone: TrackTone): string {
  return { neutral: 'cancelled', progress: 'allocating', attention: 'expiring', success: 'succeeded', error: 'failed' }[tone]
}
