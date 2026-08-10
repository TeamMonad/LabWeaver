export const SCENES = [
  {
    id: 'opening', role: 'system', seconds: 30, label: '平台总览',
    beats: [
      {atSeconds: 0, title: 'LabWeaver', body: 'Agent 驱动的云原生实验平台，将材料、审批、环境、权限、资源和证据连接成一条可追溯旅程。'},
      {atSeconds: 10, title: '统一控制面', body: '六个领域服务以 PostgreSQL 权威状态和 NATS JetStream 事件协作，失败边界清晰、状态可重放。'},
      {atSeconds: 20, title: '双运行时', body: 'Container 与 KubeVirt VM 共用环境契约，由专用 Executor 隔离高权限操作和运行时故障域。'},
    ],
  },
  {
    id: 'teacher-authoring', role: 'teacher', seconds: 45, label: '教师编排与发布',
    beats: [
      {atSeconds: 0, title: '材料进入受控流程', body: '教师上传公开安全材料，并先核对 LLM 出站策略与受保护内容边界。'},
      {atSeconds: 15, title: 'Agent 生成候选', body: 'AgentRun 分别生成 Environment 与 Evaluation 候选；确定性工具负责验证，候选不会自动执行。'},
      {atSeconds: 30, title: '人类审批后发布', body: '教师独立审阅、填写理由并批准，不可变 release 保留来源、验证结果和 Runtime 身份。'},
    ],
  },
  {
    id: 'admin-resource', role: 'platform-admin', seconds: 25, label: '资源审批与 Lease',
    beats: [
      {atSeconds: 0, title: '正式资源申请', body: '管理员处理由 Work/Agent 主路径预先产生的请求，不伪造当前不存在的科研申请入口。'},
      {atSeconds: 12, title: 'Quota 与 Lease 对账', body: '批准结果回读 ResourceQuota 和限时 Lease，使算力分配具备范围、期限和审计链。'},
    ],
  },
  {
    id: 'student-container', role: 'student', seconds: 50, label: 'Container 实验',
    beats: [
      {atSeconds: 0, title: '按模板创建环境', body: '学生从教师已发布的模板创建 Container，Environment Service 持有生命周期权威状态。'},
      {atSeconds: 13, title: '限时授权进入 xterm', body: '环境 ready 后签发 AccessGrant；网络可达与业务授权分离，终端入口不会绕过授权。'},
      {atSeconds: 26, title: '公开样例完整运行', body: '在浏览器终端中编辑、编译并运行 C++ 样例，运行环境、材料和 release 身份保持一致。'},
      {atSeconds: 39, title: '可恢复的交互体验', body: 'resize、断开和重连仍回到同一环境，控制面持续记录会话与环境生命周期。'},
    ],
  },
  {
    id: 'student-kubevirt', role: 'student', seconds: 55, label: 'KubeVirt Linux 实验',
    beats: [
      {atSeconds: 0, title: '同一课程的真实 VM 路径', body: 'KubeVirt Provider 将 Linux 系统实验纳入与 Container 一致的 Environment 契约。'},
      {atSeconds: 14, title: '受控 noVNC 入口', body: 'ConsoleCapability 将短期授权转换为浏览器控制台连接，不直接暴露 VMI 端点。'},
      {atSeconds: 28, title: '生命周期与身份保持', body: 'stop/start 或恢复由专用 Executor 调谐，并核对恢复前后的环境身份。'},
      {atSeconds: 42, title: 'Fixture 边界', body: '本预演明确展示 upstream unavailable；真实 VMI/noVNC 成功仍由 #126 connected 验收提供。'},
    ],
  },
  {
    id: 'submission-freeze', role: 'student', seconds: 25, label: '冻结与不可变证据',
    beats: [
      {atSeconds: 0, title: '冻结工作区', body: '提交动作协调双运行时冻结，并生成不可变的 FrozenSubmission 引用。'},
      {atSeconds: 12, title: '证据可以对账', body: 'Object Version、SHA-256、source commit 和 runtime identity 让结果能够复核而非依赖截图。'},
    ],
  },
  {
    id: 'access-revoke', role: 'student', seconds: 25, label: '访问撤销',
    beats: [
      {atSeconds: 0, title: '撤销立即改变能力', body: 'AccessGrant revision 更新后，已有会话终止，旧能力不再有效。'},
      {atSeconds: 12, title: '失败必须清晰', body: '再次连接返回稳定 diagnostic；策略、身份或版本不匹配时统一 fail closed。'},
    ],
  },
  {
    id: 'cleanup', role: 'student', seconds: 15, label: '环境清理',
    beats: [
      {atSeconds: 0, title: '安全回收', body: '删除环境并检查无残留终态，完成从教学编排、运行到证据与资源回收的闭环。'},
    ],
  },
] as const;

export type Scene = typeof SCENES[number];
export type SceneId = Scene['id'];
export type Profile = 'fixture-preview' | 'connected-final';
export type Cut = 'preview' | 'final';

export const VIDEO = {width: 1920, height: 1080, fps: 60} as const;
export const TOTAL_SECONDS = SCENES.reduce((sum, scene) => sum + scene.seconds, 0);
export const SCENE_IDS = new Set<string>(SCENES.map(({id}) => id));

export type FrozenIdentity = {
  packageSha256: string;
  configurationSha256: string;
  migrationCatalogSha256: string;
  deploymentSha256: string;
  imageDigests: string[];
  runtimeArtifacts: string[];
};

export type FileEvidence = {path: string; sha256: string; bytes: number};
export type CaptureReceipt = {
  schemaVersion: 'demo-video-capture-receipt.v1';
  sceneId: SceneId;
  role: Scene['role'];
  profile: Profile;
  releaseEligible: boolean;
  sourceCommit: string;
  runId: string | null;
  identity: FrozenIdentity | null;
  clip: FileEvidence & {durationSeconds: number};
  trace: FileEvidence;
  screenshot: FileEvidence;
  browser: {name: 'chromium'; version: string; playwrightVersion: string};
  viewport: {width: 1920; height: 1080};
  capturedAt: string;
  privacy: {
    automatedScan: 'passed'; humanReview: 'pending' | 'passed'; containsSecrets: false;
    containsRawUserContent: false; containsTerminalTranscript: false; containsAbsolutePaths: false;
  };
};
