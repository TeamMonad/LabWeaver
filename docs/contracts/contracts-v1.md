# 公共契约

crates/contracts 是领域类型、校验、HTTP/SSE、NATS 和 Schema 的语义来源。OpenAPI、JSON Schema 和 Web SDK 从同一来源生成，不手工维护平行类型。文件名和现有 API 版本不代表承诺保留旧消费者；v3 按已批准范围同步更新仓库内调用方。

## 归属与权限

Project 是 Work、资源申请和费用的必选归属，Course 为可选教学关联。教学课程自身的接口仍保留课程语义。Project 授权只依赖相应项目成员关系；Environment 授权使用所属 Project、可选 Course、实例和运行代次。

内部身份来自验证过的服务 JWT，不来自 actor、role 或 principal 请求头。受限服务代表用户调用时，Access 仍检查真实会话、成员及资源范围。

## 生命周期和结果

环境、评测和 Agent 任务保留各自权威状态。长任务返回可查询操作及真实阶段，取消和终态由实际 Owner 推进。刷新、重试、重复事件和进程重启通过幂等与代次条件更新处理。

资源目标区分长期环境和一次性任务；目标身份由实际 Owner 绑定项目后申请资源，不凭空创建长期 Work。GPU class 由资源目录解析成独占、时间片或 vGPU，不能由调用者篡改成另一种模式。

费用包含显式单位、费率版本、计量区间与调整原因。未知计量或缺少费率不能输出已结算的零费用。冻结文件与实时 Probe 分开表示，基础设施错误不当作学生失败，LLM 不改变确定性成绩。

## 任务进度查询

`GET /api/v1/projects/{projectId}/agent-runs/{runId}/task-progress` 使用原 AgentRun 读取权限。`GET /api/v1/projects/{projectId}/me/evaluation-tasks/{runId}` 使用学生自己的评测读取权限，并在 Evaluation 检查项目与 actor；其他人的 run 返回 404。两者只读，不授予新的取消或重试权限。

学生从 Freeze 接受响应取得 submissionId 后，可用 `GET /api/v1/projects/{projectId}/me/evaluation-tasks/submissions/{submissionId}` 查询最近创建的所属评测 Run，返回同一进度 DTO。异步任务尚未创建时返回 404；其他项目或其他 actor 的提交也返回 404。该查询不创建任务，重试同一 URI 即可刷新。

响应以 `current` 返回每个 track/step 的当前尝试，以 `history` 分页返回旧尝试。查询只接受 `cursor` 与 `limit`（默认 50，范围 1–100），删除 `page`、`pageSize` 和 `hasMore`，不保留 offset 兼容入口。响应 `nextCursor=null` 表示历史末页；续页保留各 track/step 的当前尝试，不能把旧失败显示为当前进度。Agent 的同一业务尝试可能包含多个执行代次，旧代次留在历史；Evaluation 未领取的步骤以 attempt 0 表示，尚无 TaskRunId。

游标沿用服务端 HMAC-SHA256、规范化 base64url JSON 和 current/previous 密钥轮换，签名覆盖版本、key ID、端点、actor、项目、run，以及提交查询的 submissionId。跨范围或篡改返回 400 / `LW_TASK_PROGRESS_PAGE_INVALID`。每次请求重新验证当前会话与项目权限，Evaluation 也在 SQL `LIMIT` 前限定项目、actor 和 run；游标不授予访问权限。Agent 按不可变 `(attempt, executionGeneration, track)` 降序，Evaluation 按 PG 原始精度 `(created_at, step_run_id, attempt)` 降序读取 `limit+1`；不回查仍存活的上一页末项。删除锚点不阻断续页，边界之前的新历史需刷新首页。这是当前事实的 keyset 遍历，不是跨 HTTP 数据库快照；提交关联了较新的 run 时旧续页失败，重新从首页读取。

Web 进度消费者通过共享 cursor helper 读完所有历史页，再发布本次完整读取；缺失/空串/非法类型或重复 `nextCursor` 失败关闭，不把部分历史计作完整历史。保留的上一份成功进度仍明确为旧读取，读取失败不会冒充新进度。

Work 配置脚本执行沿用现有环境和 Agent 已保存的执行意图；进度显示该阶段，但不补造新的 sandbox TaskRunId。生成阶段的旧执行代次保留在历史。

`phase` 来自业务检查点和 Resource 权威事实，区分等待批准、分配、执行和待清理。`diagnosticCode` 保留当前阻塞或失败原因；`taskRunId` 和 `resourceRequestId` 关联真实记录。`nextSteps` 只列已有操作 ID，客户端仍按项目、角色及服务端状态检查使用；没有公共入口的 Evaluation 取消/重试不会以可用按钮列出。

进度读取不返回命令、凭据、评分输入、分数或 Kubernetes 对象。Resource 绑定缺失或暂时不可用返回 503 / `LW_TASK_PROGRESS_RESOURCE_UNAVAILABLE`，不返回旧失败作为新进度。读取时 owner 改变则有界重读，仍不稳定时返回 409 / `LW_TASK_PROGRESS_CHANGED`。响应使用 `Cache-Control: no-store`，事件用于提示刷新，不能替代权威查询。

这两个接口读取指定任务，不新增全项目任务聚合。项目 AgentRun 与 Resource 项目申请列表使用下文的稳定游标，并保留各领域的权威状态。客户端不能把单页或可见项数量当作全项目完整任务数。

## 格式与错误

标识使用现有领域 UUID 类型，revision 和业务序列继续使用既有约束。输入拒绝冲突、未知字段、路径越界和非法数值。错误使用可读 Problem Details 与稳定 diagnostic，保留足够上下文但不记录 Secret 或用户正文。

内容摘要和镜像 digest 用于真实输入与产物完整性；运行时不要求开发证明包或固定镜像数量。生成与校验命令以当前 xtask 和 Web scripts 为准，修改公共契约必须重新生成并检查所有消费者。

材料上传的查询、取消、恢复和限额见[材料上传与内容身份](material-uploads.md)。学生冻结提交继续使用[Collector 契约](submission-collector-v1.md)。
项目 Credits 账户、预占与不可变账目接口见 [项目 Credits](project-credits.md)。Rust 真源位于 `resource::credits`，Schema/OpenAPI 与 Web SDK 从此真源生成。
当前用户通知、已读状态、提醒与邮件投递边界见 [通知契约](notifications.md)。

## 项目进行中列表

`GET /api/v1/projects/{projectId}/agent-runs` 和
`GET /api/v1/projects/{projectId}/resource-requests` 保留各领域服务的权威状态，不做跨服务聚合。
两者均支持重复的 `state` 查询参数、`limit`（1–100，默认 50）和不透明的 HMAC 签名 `cursor`；响应统一为
`{items, nextCursor}`。游标签名覆盖项目范围、状态过滤、快照上界以及排序键
（`createdAt DESC, id DESC`），下一页不得复用其他过滤条件的游标；首次查询的快照上界由数据库
查询开始时的 `clock_timestamp()` 捕获。

AgentRun 游标绑定项目和状态过滤；ResourceRequest 游标另外绑定可选课程和 actor 范围。非管理员资源申请列表
只返回当前会话 actor 自己提交的申请，管理员才可读取项目内全部申请。非法、篡改或跨范围游标返回
`LW_AGENT_RUN_HISTORY_CURSOR_INVALID` 或 `LW_RESOURCE_REQUEST_CURSOR_INVALID`；项目权限失败仍返回
`LW_ACCESS_DENIED`。

## Access 与 Evaluation 列表续页

Access 的 SSH 公钥列表和环境 AccessGrant 列表复用 `{items, nextCursor}`。
`limit` 范围为 1–100，默认 50。把响应中的非空 `nextCursor` 原样传给下一次请求；
`nextCursor=null` 表示本次读取没有后续记录。不要从公开的毫秒时间戳自行构造游标。
SSH 公钥按数据库 `created_at ASC, key_id ASC` 排序；AccessGrant 按
`created_at DESC, grant_id DESC` 排序。数据库先限制当前 actor、环境、状态与 endpoint，
再取页，因此其他用户的记录和不匹配 endpoint 的记录不占页容量。
游标绑定 actor 及过滤条件，不能跨用户、环境或过滤条件重用。非法游标返回
`LW_CONTRACT_DOCUMENT_INVALID`。这两类游标不是新的授权凭据；每次查询仍应用真实会话权限。

Evaluation release 列表按 `published_at DESC, release_id DESC` 排序；学生结果列表按
不可变的 `created_at DESC, run_id DESC` 排序，而不是会随重评、清理更新的 `updated_at`。
游标保存完整的数据库排序键和查询范围，不依赖签发游标的那条记录继续存在或仍为 active。
原 release 被撤回后，后续页仍可继续。学生结果游标不能跨 actor 复用。

这些续页是 keyset 读取，不是跨 HTTP 请求保持的数据库事务快照。并发删除的记录不再返回；
新增记录若排在已访问边界之前，需要刷新列表才能看到。并发新增/删除不会移动既有记录的排序键，
也不会因为删除上一页末项而重复或跳过仍符合过滤条件的记录。状态或权限改变后，每页采用当前权限，
不承诺重放已失效的记录。Agent 历史游标同样保留 PostgreSQL 微秒精度，公开 DTO 仍使用毫秒。

## Resource 管理、目录与账务列表

以下 GET 接口统一使用 `cursor` 与 `limit`（默认 50，范围 1–100），返回 `{items, nextCursor}`：

- `/api/v1/resource-requests`：平台管理员的全局申请。
- `/api/v1/resource-leases`：平台管理员的全局租约。
- `/api/v1/projects/{projectId}/resource-leases`：项目租约；保留可选 `courseId`，非管理员仅可见本人申请的租约。
- `/api/v1/resource/rates`、`/api/v1/resource/gpu-catalog`：已授权调用者可读的费率/GPU 目录历史。
- `/api/v1/projects/{projectId}/charges`、`/api/v1/projects/{projectId}/usage`：平台管理员的项目费用与用量。
- `/api/v1/projects/{projectId}/credits/ledger`、`/api/v1/projects/{projectId}/credits/holds`：当前项目成员或平台管理员可读的账目与预占。

这些接口不再返回无界数组，也不接受 `page`/`pageSize`。各集合按 Resource0024 生成的不可变唯一 `ordinal DESC` 排序；目录的显示分组或费率生效时间不作为续页键。迁移时已有记录获得稳定序号，但不承诺用该序号还原历史提交时序。每页在 SQL `LIMIT` 前应用项目、课程和 owner 过滤，并读取 `limit+1` 判断是否确有下一页。末页返回 `nextCursor=null`，不额外提供空续页。

客户端只把显式 `nextCursor=null` 视为末页；续页值必须为非空字符串。缺失游标、空字符串、布尔值、数值、数组或对象均为无效响应，显示中文诊断并拒绝整次集合读取，不把已读的部分费率或 GPU 目录当作完整结果。重复游标同样报错，不循环读取或静默终止。

复用 Resource 已有 canonical base64url/HMAC-SHA256 编码、当前/上一把签名密钥和轮换规则。签名绑定集合、项目、课程和 owner 范围，篡改或换范围重用游标返回 `400/LW_RESOURCE_REQUEST_CURSOR_INVALID`；每次查询仍验证真实身份及当前权限。游标保存序号，不回查上一页锚点。删除锚点、同时间戳、状态更新不改变剩余行的续页位置。并发插入分配的新序号排在边界之前，要刷新首页才能看到；这不是跨 HTTP 请求的事务快照，不承诺包含尚未提交的并发写。账务流水仅做只读分页，不合并、不重算、不删除原有账目。项目申请列表保持上一节已有的时间+ID快照游标，不改变其状态过滤契约。

## Environment 清理身份检查点

持久化及内部 NATS 使用的 `EnvironmentOperation` 包含可选 `cleanupTarget`：namespace、原 UID
及 shared namespace 待删 Work 对象的 `ExecutionObjectRef`。新接受的操作持久写入
`cleanupProtocolVersion=1`，证明首次 DELETE 必须先观察、经 owner lease/CAS 保存身份。
缺少标记且没有绑定的旧 Accepted/Running/Cancelling 操作和已排定重试只诊断，不得接管后重新捕获。
这两个字段不属于客户端清理授权参数，也不会出现在公开环境详情或公开操作快照中。
详情使用显式 `PublicEnvironmentInstance`/`PublicEnvironmentOperation` 投影；Access 对公开字段
继续复用权威 aggregate 的校验，不重建或推导私有清理身份。Schema/SDK 从 Rust 真源生成，
列表接口、分页响应及操作状态枚举不变，前端继续使用公开快照与已有诊断。

