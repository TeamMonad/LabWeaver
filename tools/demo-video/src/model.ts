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
      {atSeconds: 3, title: 'Agent 生成候选', body: 'AgentRun 分别生成 Environment 与 Evaluation 候选；确定性工具负责验证，候选不会自动执行。'},
      {atSeconds: 6, title: '教师独立审批', body: '候选差异、验证结果和理由同时呈现，教师分别决定 Environment 与 Evaluation 是否可以发布。'},
      {atSeconds: 9, title: '不可变发布', body: '发布后的 release 保留来源、验证结果和 Runtime 身份，后续环境不能静默漂移到另一候选。'},
      {atSeconds: 20, title: 'Agent 不替代人类决策', body: 'LLM 只提供候选与建议；教师审批和确定性验证共同守住生产执行边界。'},
    ],
  },
  {
    id: 'admin-resource', role: 'platform-admin', seconds: 25, label: '资源审批与 Lease',
    beats: [
      {atSeconds: 0, title: '正式资源申请', body: '管理员处理由 Work/Agent 主路径预先产生的请求，不伪造当前不存在的科研申请入口。'},
      {atSeconds: 1, title: '有理由的审批', body: '管理员核对申请范围、填写操作理由并确认，使资源决策具备明确的责任与审计边界。'},
      {atSeconds: 3, title: 'Quota 与 Lease 对账', body: '批准结果回读 ResourceQuota 和限时 Lease，使算力分配具备范围、期限和审计链。'},
      {atSeconds: 12, title: '云原生资源治理', body: 'Namespace、Quota 与 Lease 把共享集群容量转换为可审计、可过期、可回收的课程资源。'},
    ],
  },
  {
    id: 'student-container', role: 'student', seconds: 50, label: 'Container 实验',
    beats: [
      {atSeconds: 0, title: '按模板创建环境', body: '学生从教师已发布的模板创建 Container，Environment Service 持有生命周期权威状态。'},
      {atSeconds: 1, title: '限时 AccessGrant', body: '环境 ready 后签发 AccessGrant；网络可达与业务授权分离，终端入口不会绕过授权。'},
      {atSeconds: 2, title: '进入浏览器 xterm', body: 'ConsoleCapability 将环境、端点、角色和期限绑定后，学生才获得交互终端。'},
      {atSeconds: 3, title: '公开样例完整运行', body: '在浏览器终端中编辑、编译并运行 C++ 样例，运行环境、材料和 release 身份保持一致。'},
      {atSeconds: 7, title: '可恢复的交互体验', body: 'resize、断开和重连仍回到同一环境，控制面持续记录会话与环境生命周期。'},
      {atSeconds: 20, title: '教学环境一致性', body: '镜像 digest、材料 hash 与 release identity 让教师准备的环境能够被学生稳定复现。'},
      {atSeconds: 35, title: '运行面独立伸缩', body: 'Container Executor 可独立扩展和恢复，不必把 Kubernetes 权限扩大到整个控制面。'},
    ],
  },
  {
    id: 'student-kubevirt', role: 'student', seconds: 55, label: 'KubeVirt Linux 实验',
    beats: [
      {atSeconds: 0, title: '同一课程的真实 VM 路径', body: 'KubeVirt Provider 将 Linux 系统实验纳入与 Container 一致的 Environment 契约。'},
      {atSeconds: 1, title: '限时控制台授权', body: 'AccessGrant 与 ConsoleCapability 共同约束 VM 入口，网络可达不能替代业务授权。'},
      {atSeconds: 2, title: '受控 noVNC 入口', body: 'ConsoleCapability 将短期授权转换为浏览器控制台连接，不直接暴露 VMI 端点。'},
      {atSeconds: 7, title: '生命周期与身份保持', body: 'stop/start 或恢复由专用 Executor 调谐，并核对恢复前后的环境身份。'},
      {atSeconds: 10, title: 'Fixture 边界', body: '本预演明确展示 upstream unavailable；真实 VMI/noVNC 成功仍由 #126 connected 验收提供。'},
      {atSeconds: 24, title: '双运行时统一治理', body: 'Container 与 VM 复用同一领域状态机，同时保留各自 Provider 与故障域。'},
      {atSeconds: 40, title: '系统实验真实隔离', body: 'KubeVirt 提供真实 guest kernel 边界，适合 Container 无法替代的 Linux 系统实验。'},
    ],
  },
  {
    id: 'submission-freeze', role: 'student', seconds: 25, label: '冻结与不可变证据',
    beats: [
      {atSeconds: 0, title: '定位同一环境', body: '冻结前先加载已发布环境和限时授权，避免把另一运行实例的状态混入提交。'},
      {atSeconds: 2, title: '证据可以对账', body: 'Object Version、SHA-256、source commit 和 runtime identity 让结果能够复核而非依赖截图。'},
      {atSeconds: 12, title: '不可变证据链', body: '结果通过版本化 Schema 与逐文件 checksum 对账，弱证据不能改名冒充正式发布证据。'},
    ],
  },
  {
    id: 'access-revoke', role: 'student', seconds: 25, label: '访问撤销',
    beats: [
      {atSeconds: 0, title: '签发限时能力', body: '学生进入环境前必须取得与角色、端点和期限绑定的 AccessGrant。'},
      {atSeconds: 1, title: '已有会话受控', body: '控制台会话持续关联授权 revision，而不是只在首次连接时检查一次。'},
      {atSeconds: 2, title: '撤销立即生效', body: 'AccessGrant revision 更新后会话终止；再次连接返回稳定 diagnostic 并 fail closed。'},
      {atSeconds: 10, title: '授权与网络分层', body: '即使网络仍可达，失效的业务授权也不能建立新连接。'},
    ],
  },
  {
    id: 'cleanup', role: 'student', seconds: 15, label: '环境清理',
    beats: [
      {atSeconds: 0, title: '选择回收目标', body: '清理操作绑定明确的 Environment identity，避免误删其他课程或学生的环境。'},
      {atSeconds: 1, title: '确认删除', body: '显式确认触发受控生命周期转换，Provider 负责回收运行时资源。'},
      {atSeconds: 3, title: '读取清理终态', body: '重新读取返回稳定的不存在诊断，完成从教学编排到证据与资源回收的闭环。'},
      {atSeconds: 8, title: '资源生命周期闭环', body: '创建、授权、运行、冻结、撤销与清理都由可观察状态驱动，不依赖人工口头确认。'},
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
