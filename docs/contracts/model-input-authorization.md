# 模型输入授权

Agent 配置生成和已有 Work 配置操作只使用调用方显式选择的材料包文件。项目成员的对象动作权限仍由 Control 的成员事实和现有 `authorize_operation` 决定，敏感内容分类不能代替文件许可。

## 创建请求

`CreateAgentRunRequest` 和 `CreateWorkConfigurationRunRequest` 增加 `selectedPaths`。它是材料包内精确、规范化的相对路径数组。缺少字段、`null`、空数组、重复路径或包内不存在的路径均拒绝，诊断为 `LW_MODEL_INPUT_SELECTION_REQUIRED`；服务端不默认选择整个包。类型保持可选，以便前端独立交接，但没有选择的旧调用不能开始模型生成。

选择只属于请求的项目、材料包及版本。Control 校验当前创建动作权限、项目状态、材料和策略版本，再构造内部复核上下文。Agent 验证上下文与命令的 actor、project、course、用途、材料/策略版本和规范化选择摘要逐项一致。未选文件不进入对象读取、分类、stdin metadata、sandbox materials 或构建材料化输入。服务不根据文件名猜测隐藏属性。

## 内部实时复核

`ModelInputAuthorizationContext` 表达首次已鉴别请求的会话 SHA、发起者、项目、课程、原 operation 和当次 decision 的 `validUntil`。它不包含文件身份或许可布尔值；冻结请求尚无归档时可保存这一完整证据。`validate()` 只检查传输身份格式，不授权操作，也不把旧 decision TTL 当任务期限。现有材料包 binding 可通过 `context()` 提取同一证据；Control 构造和复核入口及 Agent 命令校验使用相同检查。

Access 从原已验证 BFF 请求、真实项目/课程授权 decision 和环境 AccessGrant 构造冻结 context，并覆盖私有转发 header；浏览器自报值不进入下游。Evaluation 首次保存 context，重放不换会话。缺失记录不得从冻结 owner、服务身份或新会话补造。冻结完成后，Evaluation 使用现有 `control.authoring.read` 调用 `POST /internal/v1/authoring/frozen-model-input-authorizations`，提交原 `inputAuthorizationContext`、task/freeze/release ID、release revision 和精确 `selectedPaths`。Control 从保护的冻结事实与现行发布材料重建 binding。Evaluation 在每次 archive/rubric 对象读取前和准备完成后复核，并把返回 binding 原样填写必需的 `InternalAgentLlmReviewRequest.inputAuthorization`。

binding 的 `source.kind` 区分 `problem_package` 和 `frozen_submission`。冻结来源分别保留 manifest revision/SHA、archive 精确 ArtifactRef/content SHA、Evaluation release revision、Environment release version，以及独立 rubric package/version/path/ArtifactRef/SHA。冻结 ID 不作为材料包 ID。策略全文 SHA 和选择摘要也参与一致性校验。

冻结来源和创建 binding 请求必需 `parentLeaseSha256`，由 Evaluation 对真实父 `EvaluationStepLease.lease_token()` 的规范 UUID 字符串 UTF-8 计算 SHA-256。它固定进入原 binding/选择摘要；恢复不能刷新，旧缺失字段不补造。task ID 可在父 worker 恢复时保留而 lease token 轮换，因此 task ID 单独不能证明原执行代次仍有效。

任务限定 metadata 还返回从当前 task/step/attempt 关联直接读取的 `taskRunId` 和 `stepId`。Control 只使用该 `stepId` 对应的 advisory step，核对它的 include 与 rubric，不通过 selectedPaths 反查步骤；多个步骤可以读取同一文件集合。`source.stepId` 必填并纳入不可变选择摘要，Evaluation 在读取内容前将它与原 lease 和 metadata 对照，跨步骤替换即使重算摘要也会拒绝。

Control 每次创建/复核通过现有 `evaluation.control.invoke` 请求任务限定 metadata 路径 `/internal/v1/evaluation-releases/{releaseId}/frozen-submissions/{submissionId}/model-input-facts/tasks/{taskRunId}`，携带原 scope、release If-Match 及 `x-labweaver-evaluation-lease-sha256`。旧无任务路径不作 fallback，拒绝/404/错误均阻止绑定。Evaluation 在返回事实前核父 run/step/attempt 关联、当前 lease 摘要/到期、持久取消和原执行期限。摘要不是许可，不返回原 lease token。Agent 首次/repair 复核使用原摘要，自己的取消与 lease 检查仍独立执行。响应丢失时先读取原 Agent receipt，不能重新选择内容、刷新截止或重复发送。

课程当前教师兼 instructor 且具有材料项目 Edit 权限时，可以通过 `POST /api/v1/projects/{projectId}/llm-egress-policies/active/rubric-rules` 单独设置 rubric 可见性、批准目标模型使用或撤回批准。GET 同路径返回教师可见的元数据规则。请求必须提供现行 policy 的 If-Match 和 Idempotency-Key。规则保存在现有 policy 的版本化 JSON 内，批准者和课程成员 revision 从当前真源获取；请求不能自报许可。每次改变可见性都会清除模型批准。只有 `course_public` 且存在当前 instructor 的有效批准时才允许外发；hidden、未知、离任、成员 revision 变化或撤回均拒绝。Browse 和模型许可分别检查。

冻结实时复核使用 `POST /internal/v1/authoring/model-input-authorizations/resolve`（现有 `control.authoring.read`）。Control 重核原会话、冻结 Edit、对象 Browse、ConfigureAgent 和当前 rubric 规则，重建完整 binding 后做全等比较。Agent 首次及 repair 进程调用前均复核，保留原任务截止和累计用量；取消完成与中断恢复保留已经观测到的用量，不重放旧模型调用。

`POST /internal/v1/model-input-authorizations/resolve` 仅在内部 API 中提供，服务账户需要现有 `control.agent_run.read` 权限。公开浏览器 API 和 SDK 不提供该操作。

请求是 `ModelInputAuthorizationResolveRequest`：

- `binding`：Control 从真实已验证 BFF 会话构造的 `sessionSha256`、actor/project/course、原创建 operation、创建当次 decision 的 `validUntil` 证据、材料/策略版本和选择摘要。
- `selectedPaths`：本次准确的文件选择。

绑定不是独立授权记录，也不保存旧角色。Control 每次读取 Access 的 `current_authorization_sessions` 有效会话及当前角色，并读取当前成员事实，调用现有 `authorize_operation`。项目归档、对象版本或归属不匹配、选择摘要变化、会话过期/撤销、角色或成员权限不足、读取失败和超时均拒绝。成功返回相同 binding；拒绝返回 403、`LW_MODEL_INPUT_AUTHORIZATION_DENIED`。

创建幂等身份绑定稳定业务意图：actor、project/course、公开请求、用途、材料/策略及其版本、对象 locator、预授权和原 operation。动态 `sessionSha256` 和授权 decision 的 `validUntil` 不属于创建意图。同一用户以相同请求和幂等键重放时，Control 仍先检查本次当前授权，然后返回原 run；首次 dispatch JSON、凭据、dispatch 摘要和已经准备的 input 摘要都不被更新。其他用户或不同请求、文件选择、对象版本复用同一键仍冲突。重放不能恢复已撤销的原会话、替换原会话或重置原任务预算。

Agent 在现有 dispatch `request` JSON 中保存 `{ request, inputAuthorization }`，不新增表、列或权限来源。缺少上下文的旧 dispatch 被逐条拒绝，不推导或回填授权。Agent 准备材料前后复核，且每次实际进程调用（包括 schema repair）前重新复核原有持久化取消状态和 Control 当前授权。准备期间取消能停止准备；读完后取消或撤权也不能启动首次模型调用。运行中的原取消机制继续负责进程停止和清理。恢复或重试派发在准备阶段被拒绝时，沿既有 claim/recovery 规则终止本次可执行动作，保留旧 attempt 的用量和 checkpoint；已完成或仍有有效租约的动作，以及已有 Work execution，不被改写。拒绝不代表旧进程已经停止或清理。需要新的会话或材料选择时，调用方必须新建显式请求；服务不更新旧绑定。

Access 的短期 authorization decision TTL 只证明创建当次的判断，不作为整个异步任务的截止。排队后的首次进程调用及每次 schema repair 进程调用前，Control 都重新检查首次绑定的原会话仍存在、未撤销、absolute/idle 期限未到，以及当前角色、成员动作权限和对象版本仍许可；不接受新会话替代原会话，也不更新首次 binding。当前会话期限从 PostgreSQL 微秒精度向下截断到契约毫秒精度；数据库到期筛选保持原精度，不延长会话期限。Agent 使用既有 attempt 的固定 `startedAt + policy.budget.timeoutMilliseconds` 截止和剩余预算，repair 不重置它们。任务期限到达时只允许既有终态恢复读取，不能启动新的发送；取消仍阻止新的发送。

## 部署连接

Agent 复核客户端读取下列四项现有命名的配置：

- `LABWEAVER_CONTROL_SERVICE_BASE_URI`：HTTPS origin；不接受用户信息、业务路径、query 或 fragment。
- `LABWEAVER_CONTROL_SERVICE_CA_PATH`：绝对路径，目标为普通文件，最多 64 KiB；支持 Kubernetes 投影文件链接。
- `LABWEAVER_CONTROL_SERVICE_AUDIENCE`：Control audience。
- `LABWEAVER_CONTROL_SERVICE_TIMEOUT_MILLISECONDS`：1–5000 ms。

四项全缺时，仅新的模型派发以 `LW_MODEL_INPUT_AUTHORIZATION_UNAVAILABLE` 失败，Agent 其他 API、诊断及清理仍能启动。部分缺失或无效值明确使配置加载失败。复核 HTTP 拒绝重定向，只接收 200，并有界读取响应；无法获得所需 service token 或服务返回拒绝时，不启动模型。

Bundle/Helm 从同一 Control 连接来源给 Agent 提供四项配置；服务账户需要已有 `control.agent_run.read`，Control 的入口 caller allowlist 必须包含实际 Agent 身份。网络策略允许 Agent 调用 Control 的 9444/TCP。配置/角色字符串不代表实际 IdP 已创建授权，部署后仍须验证 JWT、CA/DNS 与连通性。

## Sandbox 内的工具与模型请求

Authoring sandbox 不持有上游模型凭据。唯一模型出口是 #252 的 `model_gateway`（9460）：`model_gateway.public_base_url` 使用可跨 namespace 解析的完整 Service DNS，例如 `http://agent-service.labweaver-system.svc:9460/`。sandbox 的网络只放行对象传输与 9460 网关，不直连 provider；普通 Agent 服务接口仍须服务 JWT。

每次 CLI invocation 获得签名 capability，绑定 project、run/task、attempt、原执行 lease 摘要和 authoring track；`ANTHROPIC_API_KEY` 是该 capability，不是 provider key。网关按「原会话与当前对象授权 → 全 JSON 键/值 DLP → Resource admission → 上游 → settle」处理模型请求。已有 hold 在上游发送前被拒绝时按已确认零用量释放；已经发送或结果未知的调用不会被改写成零费用。SDK retry 的 invocation/call identity 和持久执行 fence 沿用原账务实现，不能重复发送或重复扣款。

Claude 的 `PreToolUse`/`PostToolUse` hook 调用同一网关的 `/authorize` 检查。每个新模型请求重新检查原 session、当前成员/对象权限、策略与内容版本、原 execution lease、取消及固定任务截止。服务端不信任 sandbox 自报 actor/project/provider URL；已开始的发送不能撤回，撤权与取消阻止后续动作与 repair。

发送前拒绝的已确认零观察及稳定 `eventId` 先写入现有 `model_gateway_call` 持久 fence，再提交 Resource 结算。RPC 失败时记录保持 pending；gateway 启动和每 10 秒的有界对账会幂等补交，同一 callId 的重复请求也先补交这条记录，再返回原拒绝，不重新发送模型。Resource 确认后 fence 才完成，零观察保留；原会话撤销也不阻止已知事实的结算。没有证据的未知调用不会进入该零用量恢复路径，不以一小时兜底代替已确认零观察。

每批最多处理 100 条到期零观察，返回本轮成功数与失败数。单条失败按 `callId` 和错误码记录 warn 后继续本批，原 `knownZero` 不变；重试次数、上次尝试时间、下次尝试时间和错误码保存在同一 ledger 行的 `zeroRetry` 元数据。自动重试按 10、20、40、80、160、300 秒退避并封顶，未尝试记录优先，已尝试记录按上次尝试时间轮转，避免永久失败的项目占满每批队首。多副本与重启共用持久调度；结算及完成 fence 仍以原不可变零观察和稳定事件标识幂等执行，不新增迁移。

秘密分类在 JSON 解码后覆盖所有字符串值和对象键，包括明文键、Unicode 转义键和任意嵌套对象键。对象键中的私钥、AWS key、JWT、provider credential 或 task capability 与字符串值使用同一拒绝规则，不会因位于工具输入的键而绕过检查。

教师创作和私人/共享 Work 配置均只把显式选择的材料包文件放进隔离 sandbox，校验实际字节的 SHA-256；未选文件不读取、不分类、不材料化。该路径不挂载、遍历或读取 Work 用户持久盘。学生 advisory 只消费冻结 manifest/step.include/llmReadable 交集和独立获准的 rubric。不能用同目录、文件名、课程相同或付款项目权限扩大输入范围。

## 权限化诊断与回收

`GET /api/v1/projects/{projectId}/agent-runs/{runId}/diagnostics`（SDK `getProjectAgentRunDiagnostics`）先检查当前 Project Browse，再核 Agent 权威 run 的 project/course 归属。接口返回 `AgentRunDiagnostics`，带 `Cache-Control: no-store`；它不是无需鉴别的导出链接。

每条 `AgentRunDiagnostic` 包含中文原因与下一步、track/attempt、稳定诊断码、模型/运行绑定、精确材料和策略版本、已观测输入/输出摘要及用量、`createdAt`/`expiresAt`。准备失败时 `preparationFailed=true`，输入摘要和用量为 `null`；未发出的调用不会伪装成零费用或已执行。详细记录没有原始材料、provider 输出、stderr、cookie、令牌或私钥。运行错误的 Debug 输出也不包含 schema repair 正文。

详细记录默认保留 30 天。读接口立即排除到期记录；Agent 每分钟有界物理删除到期 detail rows。清理不改写 Agent 用量/checkpoint、账本、成绩或业务候选。临时进程正常退出/取消时由 TempDir 回收，崩溃遗留的 `labweaver-claude-*` 目录只有具备本服务专用 marker 且超过 30 天才会删除。未标记目录、其他临时路径和用户持久盘不在扫描范围内。Sandbox 的结果、stderr 和临时材料沿现有 owner/fence 清理，仍用于结果验证的 writer 不作为“废弃日志”删除。

## 前端消费

创建入口必须显式选择非空精确 `selectedPaths`，不能自动全选或省略；202 仅表示任务已接受。展示失败时先显示中文原因/下一步，再提供诊断码与 requestId。浏览器不构造 binding、capability 或内部 resolve 请求。教师可见 rubric 不等于获准发给外部模型，界面需分别消费可见性、批准目标模型与撤回动作。

## 同步 HTTP 错误声明

公开创建操作声明 `LW_MODEL_INPUT_SELECTION_REQUIRED` 和 `LW_MODEL_INPUT_AUTHORIZATION_DENIED`；rubric 维护操作声明 `LW_MODEL_INPUT_AUTHORIZATION_INVALID` 和 `LW_MODEL_INPUT_AUTHORIZATION_DENIED`；内部创建/复核操作声明实际同步返回的 `LW_MODEL_INPUT_AUTHORIZATION_DENIED`。这些代码进入对应 operation 的 `x-labweaver-errors`，由 Rust 真源统一生成 OpenAPI/SDK。只在异步任务中出现的 `LW_MODEL_INPUT_AUTHORIZATION_REQUIRED`、`LW_MODEL_INPUT_AUTHORIZATION_UNAVAILABLE` 不列入这些同步 Control HTTP 操作的错误列表。
