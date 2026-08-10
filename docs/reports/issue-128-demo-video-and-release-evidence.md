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
| Preview manifest SHA-256 | `sha256:b648f54031b8ab8a14d772ec1fb6a8e3694671ff3b9ae372ca8c33bb9d8fcea3` |
| Preview video SHA-256 | `sha256:6f402908556574145be363a2b93517b08fcddb82bfe47bb56c5addc276e46a20` |
| Preview media | 1920×1080、60 fps、H.264、无音轨、840 秒、607771082 bytes |
| Renderer | Remotion `4.0.507`；H.264 hardware acceleration `required`；12 Mbit/s；1 worker |
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

不展示未验证的 OJ Runner、评分能力或不存在的科研用户申请 UI；不插入架构、
工程量、Scrum 或 PPT 页面。工程实现与优势只在本报告、Schema 和可验证 manifest
中体现。

## 彩排与隐私

1. Fixture 完整流程：八段采集 receipt、Trace、截图与 hash 完整后通过；记录时间为
   `2026-08-10T07:55:34.100Z`，证据 hash 与 Preview video SHA-256 一致。
2. Fixture 全片播放：已通过 FFprobe、首/中/末 seek、双语 SRT 边界、逐文件
   checksum 和 Chromium 播放验证；记录时间为 `2026-08-10T08:08:06.952Z`，
   播放验证证据 hash 为
   `sha256:486c00044c8f77b504d7c37f92ea47b46e073240f79c79364500fb996678142e`。
3. Connected final：尚未执行；仅消费 #126 的一次冻结验收窗口，不重复 deployment/replay。

人工视觉复核覆盖八个场景的代表帧与 Container 切片起始精确帧。复核发现并
修复了循环播放重新引入浏览器导航白帧的根因：渲染现在使用 native media loop，
统一裁掉每段开头一秒，并拒绝裁切后长度不足的镜头。重复执行 `verify` 后 manifest
SHA-256 保持不变，证据对账可重现。

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
