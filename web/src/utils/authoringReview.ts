import type {
  AgentRunHistoryItem,
  AgentRunSchema,
  CandidateBuildState,
  EnvironmentCandidateViewSchema,
  EnvironmentTemplateReleaseViewSchema,
  EvaluationCandidateViewSchema,
} from '@/generated/contracts'
import { formatBytes } from '@/utils/format'

type EnvironmentSpec = EnvironmentCandidateViewSchema['candidate']['spec']
type EvaluationBody = EvaluationCandidateViewSchema['candidate']['spec']['spec']
type EvaluationStep = EvaluationBody['steps'][number]
type RunState = AgentRunSchema['state']

export type Row = { label: string; value: string; hint?: string }

// ---- Review queue -----------------------------------------------------------

export type QueueGroup = 'review' | 'generating' | 'unfinished' | 'published'

/** Only experiment authoring runs produce a package teachers approve here. */
export function isExperimentAuthoringRun(item: Pick<AgentRunHistoryItem, 'purpose'>): boolean {
  return item.purpose.kind === 'authoring' && item.purpose.environmentClass === 'experiment'
}

/**
 * Groups a run by what the teacher can do next. A run whose package already
 * has an active (not withdrawn) Environment release is listed as published;
 * the release itself is the server's record of a completed approval.
 */
export function queueGroup(state: RunState, published = false): QueueGroup {
  if (published) return 'published'
  if (state === 'awaiting_approval' || state === 'succeeded' || state === 'partially_succeeded') return 'review'
  if (state === 'failed' || state === 'cancelled') return 'unfinished'
  return 'generating'
}

export const QUEUE_GROUP_LABEL: Record<QueueGroup, string> = {
  review: '可以审核',
  generating: '生成中',
  unfinished: '未完成，需回到材料页处理',
  published: '已发布',
}

export type ReleaseView = EnvironmentTemplateReleaseViewSchema

/** Latest active Environment release per authoring run, keyed by run id. */
export function activeReleasesByRun(releases: readonly ReleaseView[]): Map<string, ReleaseView> {
  const byRun = new Map<string, ReleaseView>()
  for (const release of releases) {
    if (release.withdrawal) continue
    const current = byRun.get(release.agentRunId)
    if (!current || release.version > current.version) byRun.set(release.agentRunId, release)
  }
  return byRun
}

/**
 * Whether a release publishes exactly the candidate under review. A release of
 * an earlier candidate revision means the material or configuration changed
 * after approval and the current candidate needs its own approval.
 */
export function releaseMatchesCandidate(release: ReleaseView, candidate: { id: string; revision: number }): boolean {
  return release.candidateId === candidate.id && release.candidateRevision === candidate.revision
}

/** What the review queue read for one project, shared with the review page. */
export type ReviewQueueSnapshot = {
  items: AgentRunHistoryItem[]
  releasesByRun: Map<string, ReleaseView>
  /** Diagnostic code when releases could not be read; publication state is then unknown. */
  releaseError: string | null
  hasMore: boolean
}

export function runStateLabel(state: RunState): string {
  return ({
    requested: '已提交',
    running: '生成中',
    partially_succeeded: '部分完成',
    succeeded: '已生成',
    awaiting_approval: '等待审核',
    failed: '生成失败',
    cancelled: '已取消',
    cancelling: '取消中',
  } as Record<RunState, string>)[state]
}

// ---- Readable labels ---------------------------------------------------------

export function runtimeLabel(kind: 'container' | 'virtual_machine'): string {
  return kind === 'virtual_machine' ? '虚拟机' : '容器'
}

export function buildStateLabel(state: CandidateBuildState | null | undefined): string {
  if (!state) return '未开始'
  return ({ requested: '构建中', succeeded: '构建完成', failed: '构建失败', cancelled: '构建已取消' } as Record<CandidateBuildState, string>)[state]
}

function cpuLabel(millicores: number): string {
  const cores = millicores / 1000
  return `${Number.isInteger(cores) ? cores : cores.toFixed(1)} vCPU`
}

export function resourceRows(spec: EnvironmentSpec): Row[] {
  const { resources } = spec
  return [
    { label: 'CPU', value: cpuLabel(resources.cpuMillicores) },
    { label: '内存', value: formatBytes(resources.memoryBytes) },
    { label: '磁盘', value: formatBytes(resources.storageBytes) },
    { label: 'GPU', value: resources.gpu ? `${resources.gpu.class} × ${resources.gpu.count}` : '不使用' },
  ]
}

export function resourceSummary(spec: EnvironmentSpec): string {
  return resourceRows(spec).map((row) => `${row.label} ${row.value}`).join(' · ')
}

export function networkLabel(network: EnvironmentSpec['network']): string {
  switch (network.mode) {
    case 'allow_all': return '可以访问外部网络'
    case 'deny_all': return '不能访问网络'
    case 'restricted': return `只能访问平台允许的地址（策略 ${network.policy_binding}）`
  }
}

function protocolLabel(protocol: string): string {
  return protocol === 'ssh' ? 'SSH' : protocol.toUpperCase()
}

/** Ways a student can open the environment. */
export function entryLabels(spec: EnvironmentSpec): string[] {
  const entries = spec.entries.map((entry) => `${entry.name}（${protocolLabel(entry.protocol)}）`)
  if (spec.runtime.kind === 'container' && spec.runtime.terminal) entries.push('浏览器终端')
  if (spec.runtime.kind === 'virtual_machine') entries.push('虚拟机控制台')
  return entries
}

/**
 * Runtime permissions the candidate declares, in the words a teacher reviews.
 * Only fields present in `EnvironmentSpec.security` are listed; the contract
 * has no privileged mode or capability list yet, which the view states.
 */
export function permissionRows(spec: EnvironmentSpec): Row[] {
  const { security } = spec
  return [
    { label: '运行用户', value: security.userPolicy === 'non_root_required' ? '必须以非 root 用户运行' : security.userPolicy },
    { label: '根文件系统', value: security.rootFilesystemPolicy === 'read_only_required' ? '只读，学生只能写工作目录' : '可写' },
    { label: '提升权限', value: security.privilegeEscalationPolicy === 'deny' ? '禁止' : security.privilegeEscalationPolicy },
    { label: '公网暴露', value: security.publicExposurePolicy === 'deny' ? '禁止，只能通过平台入口访问' : security.publicExposurePolicy },
    { label: '网络', value: networkLabel(spec.network) },
    { label: '安全配置', value: security.securityProfileBinding, hint: '由管理员策略定义' },
  ]
}

// ---- Scoring -----------------------------------------------------------------

export type StepSummary = {
  id: string
  role: EvaluationStep['role']
  roleLabel: string
  points: string
  onFailure: string
  method: string
  details: string[]
}

const ROLE_LABEL: Record<EvaluationStep['role'], string> = { gate: '前置检查', score: '计分', advisory: 'AI 评语（不计分）' }

function checkerLabel(step: EvaluationStep): string {
  if (step.role === 'advisory') return '模型阅读指定文件后给出评语'
  const checker = step.checker
  switch (checker.kind) {
    case 'exact': return '输出与标准答案完全一致'
    case 'token': return '按词比较输出'
    case 'exit_code': return `退出码为 ${checker.expected}`
    case 'json_schema': return '输出符合 JSON 结构'
    case 'service_state': return `服务 ${checker.service} ${checker.expected === 'active' ? '正在运行' : '已停止'}`
    default: return '自定义检查'
  }
}

function runnerDetails(step: EvaluationStep): string[] {
  const runner = step.runner
  switch (runner.kind) {
    case 'file_assertion':
      return [`需要文件：${runner.requiredFiles.join('、')}`]
    case 'program': {
      const lines = [`${runner.phase === 'compile' ? '编译' : '运行测试'}，输入 ${runner.input}，限时 ${runner.limits.wallTimeSeconds} 秒，内存 ${formatBytes(runner.limits.memoryBytes)}`]
      for (const group of runner.testGroups ?? []) lines.push(`测试组「${group.name}」${group.maxPoints} 分 · ${group.source}`)
      return lines
    }
    case 'ansible_probe':
      return [`检查系统事实：${runner.assertions.map((item) => item.fact).join('、')}`]
    case 'llm_review':
      return [`模型可读文件：${runner.include.join('、')}`]
    default:
      return []
  }
}

export function stepSummaries(body: EvaluationBody): StepSummary[] {
  return body.steps.map((step) => ({
    id: step.id,
    role: step.role,
    roleLabel: ROLE_LABEL[step.role],
    points: step.role === 'score' ? `${step.score.max} 分` : '—',
    onFailure: step.role === 'advisory' ? '不影响分数' : step.failurePolicy === 'stop' ? '不通过则停止评测' : '继续后续步骤',
    method: checkerLabel(step),
    details: runnerDetails(step),
  }))
}

export function submissionRows(body: EvaluationBody): Row[] {
  const collector = body.submission.collector
  const rows: Row[] = collector.kind === 'workspace_snapshot'
    ? [
        { label: '收集的文件', value: collector.include.join('、') },
        ...(collector.exclude?.length ? [{ label: '不收集', value: collector.exclude.join('、') }] : []),
      ]
    : [{ label: '收集的系统状态', value: collector.facts.join('、') }]
  rows.push({ label: '提交大小上限', value: formatBytes(collector.maxBytes) })
  if (body.submission.llmReadable.length) rows.push({ label: 'AI 可读的提交文件', value: body.submission.llmReadable.join('、') })
  return rows
}

// ---- Generation history ------------------------------------------------------

export type TrackHistory = { kind: string; label: string; attempts: { number: number; state: string; diagnosticCode?: string | null }[] }

export function trackHistories(run: AgentRunSchema): TrackHistory[] {
  return run.tracks
    .filter((track) => track.kind === 'environment' || track.kind === 'evaluation')
    .map((track) => ({
      kind: track.kind,
      label: track.kind === 'environment' ? '环境' : '评分规则',
      attempts: track.attempts.map((attempt) => ({ number: attempt.number, state: attempt.state, diagnosticCode: attempt.diagnosticCode })),
    }))
}

export function attemptStateLabel(state: string): string {
  return ({ pending: '等待', running: '生成中', repairing: '修复中', awaiting_approval: '等待审核', succeeded: '成功', failed: '失败', cancelled: '已取消' } as Record<string, string>)[state] ?? state
}
