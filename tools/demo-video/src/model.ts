export const SCENES = [
  {id: 'opening', role: 'system', seconds: 20, label: '双实验旅程'},
  {id: 'teacher-authoring', role: 'teacher', seconds: 145, label: '教师编排与发布'},
  {id: 'admin-resource', role: 'platform-admin', seconds: 75, label: '资源审批与 Lease'},
  {id: 'student-container', role: 'student', seconds: 180, label: 'Container 实验'},
  {id: 'student-kubevirt', role: 'student', seconds: 210, label: 'KubeVirt Linux 实验'},
  {id: 'submission-freeze', role: 'student', seconds: 75, label: '冻结与不可变证据'},
  {id: 'access-revoke', role: 'student', seconds: 90, label: '访问撤销'},
  {id: 'cleanup', role: 'student', seconds: 45, label: '环境清理'},
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
