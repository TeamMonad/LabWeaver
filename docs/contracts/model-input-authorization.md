# 模型输入授权

Agent 配置生成和已有 Work 配置操作只使用调用方显式选择的材料包文件。项目成员的对象动作权限仍由 Control 的成员事实和现有 `authorize_operation` 决定，敏感内容分类不能代替文件许可。

## 创建请求

`CreateAgentRunRequest` 和 `CreateWorkConfigurationRunRequest` 增加 `selectedPaths`。它是材料包内精确、规范化的相对路径数组。缺少字段、`null`、空数组、重复路径或包内不存在的路径均拒绝，诊断为 `LW_MODEL_INPUT_SELECTION_REQUIRED`；服务端不默认选择整个包。类型保持可选，以便前端独立交接，但没有选择的旧调用不能开始模型生成。

选择只属于请求的项目、材料包及版本。Control 校验当前创建动作权限、项目状态、材料和策略版本，再构造内部复核上下文。Agent 验证上下文与命令的 actor、project、course、用途、材料/策略版本和规范化选择摘要逐项一致。未选文件不进入对象读取、分类、stdin metadata、sandbox materials 或构建材料化输入。服务不根据文件名猜测隐藏属性。

## 内部实时复核

`POST /internal/v1/model-input-authorizations/resolve` 仅在内部 API 中提供，服务账户需要现有 `control.agent_run.read` 权限。公开浏览器 API 和 SDK 不提供该操作。

请求是 `ModelInputAuthorizationResolveRequest`：

- `binding`：Control 从真实已验证 BFF 会话构造的 `sessionSha256`、actor/project/course、原创建 operation、创建当次 decision 的 `validUntil` 证据、材料/策略版本和选择摘要。
- `selectedPaths`：本次准确的文件选择。

绑定不是独立授权记录，也不保存旧角色。Control 每次读取 Access 的 `current_authorization_sessions` 有效会话及当前角色，并读取当前成员事实，调用现有 `authorize_operation`。项目归档、对象版本或归属不匹配、选择摘要变化、会话过期/撤销、角色或成员权限不足、读取失败和超时均拒绝。成功返回相同 binding；拒绝返回 403、`LW_MODEL_INPUT_AUTHORIZATION_DENIED`。

创建幂等身份绑定稳定业务意图：actor、project/course、公开请求、用途、材料/策略及其版本、对象 locator、预授权和原 operation。动态 `sessionSha256` 和授权 decision 的 `validUntil` 不属于创建意图。同一用户以相同请求和幂等键重放时，Control 仍先检查本次当前授权，然后返回原 run；首次 dispatch JSON、凭据、dispatch 摘要和已经准备的 input 摘要都不被更新。其他用户或不同请求、文件选择、对象版本复用同一键仍冲突。重放不能恢复已撤销的原会话、替换原会话或重置原任务预算。

Agent 在现有 dispatch `request` JSON 中保存 `{ request, inputAuthorization }`，不新增表、列或权限来源。缺少上下文的旧 dispatch 被逐条拒绝，不推导或回填授权。Agent 准备材料前后复核，且每次实际进程调用（包括 schema repair）前重新复核原有持久化取消状态和 Control 当前授权。准备期间取消能停止准备；读完后取消或撤权也不能启动首次模型调用。运行中的原取消机制继续负责进程停止和清理。恢复或重试派发在准备阶段被拒绝时，沿既有 claim/recovery 规则终止本次可执行动作，保留旧 attempt 的用量和 checkpoint；已完成或仍有有效租约的动作，以及已有 Work execution，不被改写。拒绝不代表旧进程已经停止或清理。需要新的会话或材料选择时，调用方必须新建显式请求；服务不更新旧绑定。

Access 的短期 authorization decision TTL 只证明创建当次的判断，不作为整个异步任务的截止。排队后的首次进程调用及每次 schema repair 进程调用前，Control 都重新检查首次绑定的原会话仍存在、未撤销、absolute/idle 期限未到，以及当前角色、成员动作权限和对象版本仍许可；不接受新会话替代原会话，也不更新首次 binding。Agent 使用既有 attempt 的固定 `startedAt + policy.budget.timeoutMilliseconds` 截止和剩余预算，repair 不重置它们。任务期限到达时只允许既有终态恢复读取，不能启动新的发送；取消仍阻止新的发送。

## 部署连接

Agent 复核客户端读取下列四项现有命名的配置：

- `LABWEAVER_CONTROL_SERVICE_BASE_URI`：HTTPS origin；不接受用户信息、业务路径、query 或 fragment。
- `LABWEAVER_CONTROL_SERVICE_CA_PATH`：绝对路径，目标为普通文件，最多 64 KiB；支持 Kubernetes 投影文件链接。
- `LABWEAVER_CONTROL_SERVICE_AUDIENCE`：Control audience。
- `LABWEAVER_CONTROL_SERVICE_TIMEOUT_MILLISECONDS`：1–5000 ms。

四项全缺时，仅新的模型派发以 `LW_MODEL_INPUT_AUTHORIZATION_UNAVAILABLE` 失败，Agent 其他 API、诊断及清理仍能启动。部分缺失或无效值明确使配置加载失败。复核 HTTP 拒绝重定向，只接收 200，并有界读取响应；无法获得所需 service token 或服务返回拒绝时，不启动模型。

当前依赖基线尚未为 Agent 配置上述连接及 Agent→Control 的 `control.agent_run.read` 服务角色。部署 owner 必须在 #203 的同源配置中接线并完成权限评审。本变更没有修改 identity、Bundle、Helm 或实际运行权限，也没有执行部署。

## 覆盖与交接

本切片覆盖教师材料包，以及私人/共享 Work 的材料包选择和实时项目动作权限。前端材料选择交互由 Grok 接续，当前三处旧创建入口没有传 `selectedPaths`，会明确失败。

尚未覆盖学生冻结 advisory 与隐藏 rubric、实时 Work 会话文件许可、逐文件隐藏标签、日志留存切片，以及 Claude Code 单个进程内部的 Read/Bash/tool-result 和后续模型请求逐动作复核。平台进程调用前的检查不能证明这些内部动作已逐项授权。不能据此宣称 #216 全部完成。

本地验证使用真实 PostgreSQL 当前会话/成员事实、原 Agent 持久化取消状态和合成 stdin 捕获进程，证明选择内容、准备期间取消、repair 前撤权，以及实际跨越创建当次 5 秒 decision TTL 的排队和 repair 仍按原会话当前权限运行。绝对/idle 期限到期、会话撤销、成员撤销、任务截止和取消均阻止新发送。Access 的 `idle_expires_at <= expires_at` 约束意味着绝对期限到期时 idle 也到期；测试保留该约束，并另外覆盖 absolute 仍未来而 idle 单独到期。该证据没有调用真实模型，也没有验证 Kubernetes sandbox 或生产 Agent→Control TLS/服务角色接线。
