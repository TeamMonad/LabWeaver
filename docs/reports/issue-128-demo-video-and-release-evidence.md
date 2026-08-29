# #128 演示视频与发布证据报告

## 结论

当前公开发布的 Go/No-Go 为 **No-Go / blocked**，诊断为
`LW_DEMO_VIDEO_CONNECTED_EVIDENCE_PENDING`。即使 #126 延期，当前完整 Fixture
预演版仍可作为明确标注的 Demo 视频，通过不公开链接按期交付和播放；它不能作为
发布证据或公开最终版。解除公开发布阻塞的条件是 #126 在同一冻结身份下交付全部
connected 镜头与通过的 Release Gate v3，随后由 D 完成成片 Verify 和逐帧隐私审阅。

## 构建与交付身份

| 字段 | 当前值 |
| --- | --- |
| Issue | #128 |
| Branch | `feature/128-release-demo` |
| Target | `develop` |
| PR | #164（Draft）；禁止 auto-merge |
| Fixture source commit | `6316c60557ad52edaf78d7b15b0679a5107dab84` |
| Preview manifest | `artifacts/demo-video/preview/demo-video-manifest.v1.json` |
| Preview manifest SHA-256 | `sha256:374057d4c811af745e324fefd141842a414c1e74bb88f8795112322bc5f46863` |
| Preview video SHA-256 | `sha256:92e33d242c5cb200c47dad20fdecbc4a4dda58c3ab83f4d5329ed16041cea9b8` |
| Preview media | 1920×1080、60 fps、H.264、无音轨、270 秒、204211807 bytes |
| Renderer | Remotion `4.0.507`；H.264 hardware acceleration `required`；12 Mbit/s；4 workers；source once + final screenshot |
| Local deployment report SHA-256 | `sha256:3645a3e547d8eda2931bdcb1cc539571a77079f8b0d07714c52cb12d5bd063ca` |
| #126 Run / Gate identity | 未提供，blocked |
| 归档策略 | 仅保留忽略的本地工作目录；无 CI/Release 上传、远端备份或压缩包 |

`demo-video-manifest.v1` 逐文件记录 MP4、两份 SRT、八份 capture receipt 与
输入镜头的 SHA-256。报告和 manifest 只记录仓库相对 locator、hash、计数与状态，
不记录凭据、终端正文、VNC 像素内容或本机路径。

## 场景覆盖

| Scene ID | 角色与产品路径 | Fixture 边界 | Final 退出条件 |
| --- | --- | --- | --- |
| `opening` | 同一课程的 Container 与 KubeVirt 双实验 | 产品开场预演 | connected 产品身份对账 |
| `teacher-authoring` | 材料、出站策略、AgentRun、独立审批与 release | 确定性 Fixture UI | 真实 Keycloak/服务终态 |
| `admin-resource` | 正式流程预生成请求、调整/批准、Quota/Lease | Fixture 请求与 Lease | #126 Resource readback |
| `student-container` | xterm、生命周期、重连 | 明示 in-memory terminal | 真实 Pod/PTY 镜头 |
| `student-kubevirt` | noVNC、Linux 操作、恢复身份 | 明示 upstream unavailable | 真实 VMI/RFB 镜头 |
| `submission-freeze` | Object Version、SHA-256、终态投影 | Fixture 冻结终态 | connected FrozenSubmission |
| `access-revoke` | 现有会话终止、重连拒绝 | Fixture fail-closed | connected 60 秒内终止 |
| `cleanup` | 删除与无残留终态 | Fixture not-found readback | connected cleanup readback |

不展示未验证的 OJ Runner、评分能力或不存在的科研用户申请 UI；不插入 PPT、
工程量或 Scrum 讲解页。成片使用 21 个简短解释卡说明平台总览、云原生边界和
每个产品步骤的价值，背景始终来自对应产品镜头或该镜头的最终截图。

## 彩排与隐私

1. Fixture 完整流程：八段采集 receipt、Trace、截图与 hash 完整后通过；记录时间为
   `2026-08-10T15:03:58.540Z`，证据 hash 与 Preview video SHA-256 一致。
2. Fixture 全片播放：已通过 FFprobe、首/中/末 seek、双语 SRT 边界、逐文件
   checksum 和 Chromium 播放验证；记录时间为 `2026-08-10T15:04:25.613Z`，
   播放验证证据 hash 为
   `sha256:875152b0af7b5d7d9048f40655e108bccd4a0eab8334a4ff796ef3842e59d7a5`。
3. Connected final：尚未执行；仅消费 #126 的一次冻结验收窗口，不重复 deployment/replay。

人工视觉复核覆盖八个场景的代表帧。复核确认旧版重复演示来自短素材的 native
media loop；该产物已失效。替换时间线固定为 4 分 30 秒，每段素材裁掉导航阶段后
只播放一次，随后使用同一 capture receipt 中的最终截图承载逐步解释，不再循环
任何演示动作。渲染 profile 和 chunk hash 显式记录 `mediaLoopMode=disabled` 与
`sourcePlaybackMode=once-then-final-frame`，旧切片无法复用。

自动隐私门禁扫描 manifest、字幕与文件名中的 token、内部域名、绝对路径和私有
locator。像素内容无法由文本扫描证明安全，因此 preview 维持 `humanReview: pending`，
公开 final 必须由 D 逐帧审阅后写入 `passed`。

## Reviewer 与发布边界

- B（`@zeyi2`）：证据 Schema、identity、fail-closed 与隐私边界 Review。
- C（`@yingxvemiao`）：Web 分镜、节奏、字幕与播放体验 Review。
- D（`@Nova-Lciop-J`）：#126 connected 身份、Release Gate、最终成片与隐私 Verify。
- A 负责架构与许可边界，不得以作者身份替代 B/C Review 或 D Verify。

最终 Go 需要同时满足：八个主镜头全部来自一个 #126 identity、Release Gate v3
通过、D Verify 通过、逐帧隐私审阅通过、三次彩排通过，以及报告/manifest/video
checksum 完全对账。在此之前，公开发布保持 blocked。
