import { activeScene, fixtureProjectId } from './scenes'
import type {
  AgentRunHistoryPageSchema,
  AgentRunSchema,
  EnvironmentCandidateViewSchema,
  EvaluationCandidateViewSchema,
  ProblemPackageSchema,
} from '../src/generated/contracts/types.gen'

/** Authoring review fixtures: one experiment run ready for review and older history. */
export const reviewRunId = 'run-authoring-optics'
const environmentCandidateId = 'candidate-env-optics'
const evaluationCandidateId = 'candidate-eval-optics'
const packageId = 'package-optics-v2'
const GIB = 1024 ** 3

export function authoringScene(): boolean {
  return activeScene.value.id.startsWith('teacher-review')
}

export const agentRunHistory: AgentRunHistoryPageSchema = {
  page: 1,
  pageSize: 20,
  hasMore: false,
  items: [
    { id: reviewRunId, projectId: fixtureProjectId, courseId: 'course-physics-2026', purpose: { kind: 'authoring', environmentClass: 'experiment' }, state: 'awaiting_approval', createdAt: '2026-10-11T02:10:00.000Z', updatedAt: '2026-10-11T03:42:00.000Z' },
    { id: 'run-authoring-failed', projectId: fixtureProjectId, courseId: 'course-physics-2026', purpose: { kind: 'authoring', environmentClass: 'experiment' }, state: 'failed', createdAt: '2026-10-10T08:00:00.000Z', updatedAt: '2026-10-10T08:20:00.000Z' },
    { id: 'run-authoring-running', projectId: fixtureProjectId, courseId: 'course-physics-2026', purpose: { kind: 'authoring', environmentClass: 'experiment' }, state: 'running', createdAt: '2026-10-12T01:00:00.000Z', updatedAt: '2026-10-12T01:05:00.000Z' },
    { id: 'run-work-template', projectId: fixtureProjectId, courseId: null, purpose: { kind: 'authoring', environmentClass: 'work' }, state: 'succeeded', createdAt: '2026-10-09T08:00:00.000Z', updatedAt: '2026-10-09T08:20:00.000Z' },
  ],
} as unknown as AgentRunHistoryPageSchema

const usage = { inputTokens: 18200, outputTokens: 4100, requests: 6 }

export const reviewRun = {
  id: reviewRunId,
  projectId: fixtureProjectId,
  courseId: 'course-physics-2026',
  packageId,
  policyId: 'policy-physics',
  policyRevision: 2,
  revision: 7,
  state: 'awaiting_approval',
  purpose: { kind: 'authoring', environmentClass: 'experiment' },
  tracks: [
    { kind: 'environment', candidateId: environmentCandidateId, attempts: [{ number: 1, state: 'succeeded', usage, usageObserved: true }] },
    { kind: 'evaluation', candidateId: evaluationCandidateId, attempts: [{ number: 1, state: 'failed', diagnosticCode: 'LW_EVIDENCE_INVALID', usage, usageObserved: true }, { number: 2, state: 'succeeded', usage, usageObserved: true }] },
  ],
} as unknown as AgentRunSchema

const containerArtifact = {
  kind: 'container',
  id: 'artifact-optics-env',
  build_request_id: 'build-optics-env',
  repository: 'registry.labweaver.local/course/optics-lab',
  digest: 'sha256:5f1c2b7d9a04e6c3b8f1a2d4e6c8b0a1f3e5d7c9b1a3e5f7d9c1b3a5e7f9d1c3',
}

export const environmentCandidateView = {
  candidate: {
    id: environmentCandidateId,
    projectId: fixtureProjectId,
    courseId: 'course-physics-2026',
    runId: reviewRunId,
    revision: 3,
    policyRevision: 2,
    model: 'claude-sonnet-4',
    createdAt: '2026-10-11T03:10:00.000Z',
    spec: {
      apiVersion: 'environment.labweaver.io/v1',
      kind: 'EnvironmentSpec',
      class: 'experiment',
      name: '光的干涉 · Python 数据分析实验',
      entries: [
        { name: 'jupyter', protocol: 'https', servicePort: 8888 },
      ],
      network: { mode: 'restricted', policy_binding: 'course-pypi-mirror' },
      resources: { cpuMillicores: 2000, memoryBytes: 4 * GIB, storageBytes: 10 * GIB, gpu: null },
      retention: { class: 'course_material', disposition: 'retain_until_revoked' },
      runtime: { kind: 'container', provider_binding: 'gvisor-default', service_port: 8888, build_context: { artifactId: 'ctx-1', mediaType: 'application/x-tar', objectVersion: 'v1', sha256: 'sha256:aa', sizeBytes: 1 }, terminal: { executable: '/bin/bash', workingDirectory: '/workspace' } },
      security: { privilegeEscalationPolicy: 'deny', publicExposurePolicy: 'deny', rootFilesystemPolicy: 'read_only_required', securityProfileBinding: 'restricted-v1', runAsUser: 65534, privileged: false, capabilities: [], seccomp: 'runtime_default' },
    },
  },
  approvals: [],
  build: { state: 'succeeded', artifact: containerArtifact, diagnosticCode: null },
  imageArtifact: containerArtifact,
  trustRevision: 4,
} as unknown as EnvironmentCandidateViewSchema

const runnerArtifact = { ...containerArtifact, id: 'artifact-optics-runner', build_request_id: 'build-optics-runner', repository: 'registry.labweaver.local/course/optics-runner' }

export const evaluationCandidateView = {
  candidate: {
    id: evaluationCandidateId,
    projectId: fixtureProjectId,
    courseId: 'course-physics-2026',
    runId: reviewRunId,
    revision: 2,
    policyRevision: 2,
    model: 'claude-sonnet-4',
    createdAt: '2026-10-11T03:30:00.000Z',
    spec: {
      apiVersion: 'evaluation.labweaver.io/v1',
      kind: 'EvaluationSpec',
      metadata: { name: 'optics-interference', version: '2026.10' },
      spec: {
        submission: {
          collector: { kind: 'workspace_snapshot', include: ['analysis.ipynb', 'results/fringe.csv', 'report.md'], exclude: ['**/.ipynb_checkpoints/**'], maxBytes: 16 * 1024 * 1024 },
          llmReadable: ['report.md'],
        },
        steps: [
          { id: 'files', role: 'gate', failurePolicy: 'stop', runner: { kind: 'file_assertion', requiredFiles: ['analysis.ipynb', 'results/fringe.csv'] }, checker: { kind: 'exit_code', expected: 0 } },
          { id: 'fringe-spacing', role: 'score', failurePolicy: 'continue', score: { max: 60 }, runner: { kind: 'program', input: 'results/fringe.csv', phase: 'test', limits: { memoryBytes: 512 * 1024 * 1024, outputBytes: 65536, wallTimeSeconds: 30 }, toolchainProfile: 'python-3.12', testGroups: [{ name: '条纹间距', maxPoints: 40, source: 'hidden/tests/spacing.py' }, { name: '误差分析', maxPoints: 20, source: 'hidden/tests/error.py' }] }, checker: { kind: 'token' } },
          { id: 'report', role: 'score', failurePolicy: 'continue', score: { max: 40 }, runner: { kind: 'file_assertion', requiredFiles: ['report.md'] }, checker: { kind: 'exit_code', expected: 0 } },
          { id: 'review', role: 'advisory', failurePolicy: 'continue_advisory', runner: { kind: 'llm_review', include: ['report.md'], outputMode: 'goal_assessment' } },
        ],
        aggregation: { kind: 'deterministic_sum', maxScore: 100, gates: [{ step: 'files', requiredStatus: 'passed' }] },
        review: { forceManualWhen: [], teacherApprovalRequiredForRelease: true },
      },
    },
  },
  approvals: [],
  runnerBuild: { state: 'succeeded', artifact: runnerArtifact, diagnosticCode: null },
  runnerImageArtifact: runnerArtifact,
  trustRevision: 4,
} as unknown as EvaluationCandidateViewSchema

export const reviewPackage = {
  id: packageId,
  projectId: fixtureProjectId,
  courseId: 'course-physics-2026',
  revision: 2,
  completedAt: '2026-10-11T02:05:00.000Z',
  retention: { class: 'course_material', disposition: 'retain_until_revoked' },
  files: [
    { path: 'README.md', object: { artifactId: 'a1', mediaType: 'text/markdown', objectVersion: 'v1', sha256: 'sha256:01', sizeBytes: 4200 } },
    { path: 'handout/interference.pdf', object: { artifactId: 'a2', mediaType: 'application/pdf', objectVersion: 'v1', sha256: 'sha256:02', sizeBytes: 1_240_000 } },
    { path: 'data/raw_fringes.csv', object: { artifactId: 'a3', mediaType: 'text/csv', objectVersion: 'v1', sha256: 'sha256:03', sizeBytes: 86_000 } },
    { path: 'hidden/tests/spacing.py', object: { artifactId: 'a4', mediaType: 'text/x-python', objectVersion: 'v1', sha256: 'sha256:04', sizeBytes: 3100 } },
  ],
} as unknown as ProblemPackageSchema

/** Work template authoring: one generated Work environment candidate. */
export const workRunId = 'run-work-template'
export const workRun = {
  ...reviewRun,
  id: workRunId,
  courseId: null,
  revision: 3,
  state: 'awaiting_approval',
  purpose: { kind: 'authoring', environmentClass: 'work' },
  tracks: [{ kind: 'environment', candidateId: 'candidate-env-work', attempts: [{ number: 1, state: 'succeeded', usage, usageObserved: true }] }],
} as unknown as AgentRunSchema

export const workCandidateView = {
  ...environmentCandidateView,
  candidate: {
    ...environmentCandidateView.candidate,
    id: 'candidate-env-work',
    runId: workRunId,
    courseId: null,
    spec: {
      ...environmentCandidateView.candidate.spec,
      class: 'work',
      name: '材料模拟 Work · Python + LAMMPS',
      entries: [{ name: 'jupyter', protocol: 'https', servicePort: 8888 }, { name: 'ssh', protocol: 'ssh', servicePort: 22 }],
      network: { mode: 'allow_all' },
      resources: { cpuMillicores: 4000, memoryBytes: 16 * GIB, storageBytes: 50 * GIB, gpu: { class: 'nvidia-l4', count: 1 } },
      security: { privilegeEscalationPolicy: 'deny', publicExposurePolicy: 'deny', rootFilesystemPolicy: 'mutable_required', securityProfileBinding: 'restricted-v1', runAsUser: 65534, privileged: false, capabilities: [], seccomp: 'runtime_default' },
    },
  },
} as unknown as EnvironmentCandidateViewSchema
