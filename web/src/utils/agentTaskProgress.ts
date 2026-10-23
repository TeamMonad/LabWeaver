import type { AgentRunTaskProgress, AgentTrackKind, AgentTrackTaskProgress, TaskAttemptPhase } from '@/generated/contracts'

/** Visual tone of one track's current attempt, mapped onto the shared status pill colours. */
export type TrackTone = 'neutral' | 'progress' | 'attention' | 'success' | 'error'

export interface AgentTrackReading {
  track: AgentTrackKind
  trackLabel: string
  phaseLabel: string
  tone: TrackTone
  /** Native attempt number, shown only from the second attempt on. */
  attempt: number | null
  /** The attempt needs a person: resource approval or a review. */
  needsPerson: boolean
  diagnosticCode: string | null
}

const TRACK_LABEL: Record<AgentTrackKind, string> = {
  environment: '环境',
  evaluation: '评测',
  work_configuration: 'Work 配置',
}

const PHASE: Record<TaskAttemptPhase, { label: string; tone: TrackTone; needsPerson?: boolean }> = {
  queued: { label: '排队中', tone: 'neutral' },
  preparing: { label: '准备中', tone: 'progress' },
  waiting_resource_approval: { label: '等待资源审批', tone: 'attention', needsPerson: true },
  allocating: { label: '分配资源中', tone: 'progress' },
  executing: { label: '执行中', tone: 'progress' },
  awaiting_review: { label: '等待人工审核', tone: 'attention', needsPerson: true },
  cleanup_pending: { label: '清理中', tone: 'progress' },
  succeeded: { label: '已完成', tone: 'success' },
  failed: { label: '失败', tone: 'error' },
  cancelled: { label: '已取消', tone: 'neutral' },
  blocked: { label: '受阻', tone: 'error' },
}

const TRACK_ORDER: AgentTrackKind[] = ['work_configuration', 'environment', 'evaluation']

export function readAgentTrack(item: AgentTrackTaskProgress): AgentTrackReading {
  const phase = PHASE[item.progress.phase]
  return {
    track: item.track,
    trackLabel: TRACK_LABEL[item.track],
    phaseLabel: phase.label,
    tone: phase.tone,
    attempt: item.progress.attempt > 1 ? item.progress.attempt : null,
    needsPerson: Boolean(phase.needsPerson),
    diagnosticCode: item.progress.diagnosticCode ?? null,
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

/** One sentence for the row: what a person has to do next, if anything. */
export function agentProgressAttention(tracks: AgentTrackReading[]): string | null {
  const waiting = tracks.find((track) => track.needsPerson)
  if (!waiting) return null
  return waiting.phaseLabel === '等待资源审批'
    ? `${waiting.trackLabel}正在等待资源审批，批准后自动继续。`
    : `${waiting.trackLabel}已执行完，等待人工审核后才会继续。`
}

/** A state string the shared status pill already colours as this tone. */
export function toneState(tone: TrackTone): string {
  return { neutral: 'cancelled', progress: 'allocating', attention: 'expiring', success: 'succeeded', error: 'failed' }[tone]
}
