# 环境生命周期

Environment 是交互环境的唯一业务生命周期。Experiment 与 Work 表达用途，Container 与 VirtualMachine 表达执行后端，二者独立组合，不为 xv6、CTF 或某门课程新增生命周期。

## 归属和创建

每个环境属于 Project，可关联 Course。Project 成员和环境所有者授权均需检查；课程结束不能删除独立科研 Work。

创建绑定明确模板、后端及资源意图。Resource 管理审批、分配与租约，Environment 在获得有效授权后创建运行资源。资源申请可以先于实际运行；一次性任务申请资源时无需创建长期 Environment。

默认模式由 Environment 唯一写入环境 Namespace、Quota、PVC、容器和 VM。既有 namespace 模式由部署管理员保留 namespace 及其整体策略的管理权，Environment 只管理本 Work 对象。Resource 不预建 quota shell 再让 Environment 接管。Kubernetes 对象按实例/操作代次识别，重复调用不创建第二份资源。

## 既有 namespace 运行落点

本增量支持一个部署指定项目的普通 Container Work。它仍通过 Resource 授权、lease 和 Environment 生命周期创建；客户端不能选择 namespace。VM 和 Experiment 不使用此模式。现有安全上下文保持生效，网络声明须为 `deny_all`。它不提供 privileged、额外 capabilities、root 默认值、持久盘独立回收、7天保留或导入导出；这些属于 #207 的后续范围。Namespace 不是宿主隔离或特权运行边界。

默认不设置 `LABWEAVER_CONTAINER_PLACEMENT_CONFIG_FILE`，继续使用 `lw-env-{environmentId}` 专用 namespace。启用时按以下步骤准备部署输入：

1. 在私有部署目录准备 `profile.json`，格式见 `deploy/config/container-placement.json.example`。填入已批准既有 namespace 的名称、当前 UID 和唯一项目 ID。示例中的 ID 都是合成数据。
2. 在加密 Bundle 的 `application.configMaps` 中添加可选 `container-placement-config` 对象，将 `profile.json` 引用到该私有配置文件。Bundle 从同一归档生成 ConfigMap 并派生 `containerPlacementProfileConfigMap`，Helm 将同一对象只读挂载到 Environment owner 和 Container executor。独立 Helm 部署可以直接引用管理员维护的同一 ConfigMap；Helm 本身不创建它或目标 Namespace。不要覆盖两个 workload 的文件环境变量。
3. 非 Helm 启动同样使用绝对文件路径，并给两个进程提供同一 profile。文件缺失、未知字段、非法名称、零 UID、非 Work/指定项目或不支持的网络声明明确失败。
4. 部署管理员在批准的操作范围内检查 namespace 现行 PSA、资源预算、存储类和网络隔离是否适合本 Work 及冻结 Job。Environment 不创建或覆盖 namespace 的 PSA、quota、LimitRange 或 NetworkPolicy，也不扩大 RBAC。现行 `deny_all` 依赖该专用 namespace 的部署策略；本模式不能用于未隔离的多租户共享 namespace。

每个 Work 使用 `lw-{environmentId}-runtime` 的 Deployment、Service 和 ServiceAccount，`lw-{environmentId}-workspace` 的 PVC，以及 `lw-{environmentId}-registry-pull` 的拉取 Secret。容器内名称和工作目录仍为 `runtime` 和 `/workspace`。CPU/内存/磁盘申请仍来自原 Resource 授权；模式不增加资源预算或授权范围。

Executor 在首次 Kubernetes 副作用前，与现有代次 fence 同一事务将 namespace 名称、UID 和 Work/项目身份保存为 Environment idempotency ledger 的不可变结果。重启和重放读取原结果。改变配置不能迁移原 Work，namespace 同名重建也被 UID 检查拒绝。旧版本已执行的专用 namespace Work 不能被直接采用为既有 namespace Work。无需新表或迁移；该账本结果必须与 Environment 事实一起保留。

Freeze binding 从该事实返回真实 namespace/PVC。终端、Work 配置脚本和 Access 应用代理也消费同一落点；应用代理从现行内部 endpoint eligibility 获取 `runtimeTarget`。默认模式仍返回原专用 namespace 的目标。Owner、项目、revision、Ready、观测代次、lease 和端点健康资格检查保持生效。Evaluation coordinator 由 #197 owner 组合，不能改写 binding 或跳过资格检查。

停止只退役本 Work Pod。删除先清除本 Work 的计算对象，再清除 PVC。每次对象删除都核固定名称和归属，并携带 UID/resourceVersion 前置条件。计算阶段结束前，Executor 读取完整的 namespace Pod 列表，确认本 Work 的 Pod，以及引用本 Work PVC 的 Pod 均已消失；缺少或不同归属标签的挂载 Pod 也会阻止盘清除。Pending 或 terminating Pod 仍视为存在。列表不完整、格式未知或超时均不进入盘清除，重试重新观测，不保存可复用的“已经卸载”标记。

清理不删除既有 Namespace，不移除任何 finalizer，也不删除其他 Work、部署策略或冻结 Job。未知归属、UID 变化和超时保持失败状态。Pod 消失的观测不替代存储后端的挂载 fencing。删除仍会删除本 Work PVC，七天回收站、恢复和独立磁盘清除终态尚未交付。删除操作证据区分 `namespaceRetained=true`、`workObjectsAbsent=true` 和默认模式的 `namespaceAbsent=true`。

回退此增量前，先通过当前版本正常收尾既有 namespace Work。不要切回默认模式来迁移现有 Work，也不要删除其落点账本记录。真实定位和凭据保留在私有部署输入；Issue/PR 只记录合成验证结果。

`POST /api/v1/environments` 必须携带真实 Resource 申请的 `resourceRequestId`；Work 与 Experiment
都通过原申请、审批、资金租约及自动交接创建。公开创建只附着原创建操作，不打开独立 reservation。
手工 Work 不读取项目模型配置。

已有实例的 `start`、`restart`、`retry`、`recover`、`reset` 在 HTTP 受理前重新读取原运行授权及资金覆盖；
owner 与执行器在新计算效果前再次核对。只持有旧 Environment 快照或容量租约不能授权新计算；
缺少运行授权客户端时返回 `503/LW_RUNTIME_AUTHORITY_UNAVAILABLE`，不退回仅容量校验。
重复命令沿原 Inbox/幂等账本重放，不产生第二个操作或 hold。

内部 handoff、租约刷新及 JetStream 的容量 lease verifier 仍区分业务失效与协议损坏：
未激活、非 Active、Active + `authorization:null` 或当前有效窗之外是 `409/LW_RUNTIME_LEASE_NOT_ACTIVE`；
反序列化失败、错误版本、授权身份/范围或结构非法是 `503/LW_ENVIRONMENT_LEASE_VERIFICATION_RESPONSE_INVALID`。
这些结果不替代完整运行授权，JetStream 受理的旧容量 intent 仍须通过执行前 fresh 检查。
费用估算复用已生成的 `estimateProjectResourceCost`，输入 `resources`、`computeFrom/computeUntil` 与 `storageFrom/storageUntil`，返回 `ResourceCostEstimate` 的固定费率明细和 `total`；不由 Environment 或浏览器另建计价逻辑。该接口不依赖模型，也不授予容量或资金权限，见[计算与存储估价](project-credits.md#计算与存储估价)。


### 显式升级 Work 模板

`POST /api/v1/environments/{environmentId}/template-upgrade`（SDK `upgradeEnvironmentTemplate`）
要求 `Idempotency-Key`、当前 `If-Match` 和 `{targetReleaseId,targetReleaseVersion,confirmed:true}`。
服务端先进行原有兼容性预检。跨项目/课程/模板族、非新版本或模板撤回仍由对应错误拒绝；
资源、网络、安全、provider、端口和入口变化仍以 `LW_WORK_TEMPLATE_UPGRADE_REQUIRES_RESOURCE_APPROVAL` 拒绝。
VM 总是返回 HTTP 422 `LW_WORK_TEMPLATE_UPGRADE_UNSUPPORTED_FOR_VM` 和中文原因，不隐式重建。

当前采用 C 阶段规则：绑定了原 lease 的 Work（目前所有合法 Work）即使完全兼容，也返回 HTTP 409
`LW_WORK_TEMPLATE_UPGRADE_UNAVAILABLE`。拒绝发生在原 lease 验证、Access 撤权和 store 接受事务之前；
不改 `releaseId/releaseVersion`、运行实例或工作目录，不新增 operation/Outbox，不调用 Provider。
同 key 或新 key 重试仍拒绝，不以受理一个后台必然失败的 Restart 代替同步拒绝。

替代做法是按新模板新建 Work，自行备份和迁移所需数据；检查完成后再释放旧 Work。
这不是自动转移原 lease 或磁盘，新 Work 仍走自己的正式准入与计费。

兼容升级复用原 lease 的恢复工作见 [#288](https://github.com/TeamMonad/LabWeaver/issues/288)（A'）。
现有 store 接受实现、历史 `templateUpgrade` 操作 DTO 和 timeout/cancel 的保盘语义保留，
但不作为当前公开入口支持升级的承诺。绕过入口写入升级 intent 仍会被 fresh 的精确配置 binding 拒绝，
旧 release 的运行授权不能驱动新 release。

## 状态、访问和回收

desiredState 表达请求，observedState 表达实际状态。请求受理不等于 Ready，停止请求不等于已释放。保留现有 Reconciler、持久任务、取消、租约、Outbox 和条件更新，不复制第二套状态机。

Web Terminal、noVNC、SSH/SFTP 连接使用相同 AccessGrant 边界。实例状态、端点健康、项目授权和租约均有效时才能建立新连接；撤销、到期、停止和删除触发服务端连接清理。失败不得靠页面隐藏按钮代替服务端拒绝。

`GET /api/v1/environments/{environmentId}` 返回原实例字段和 `connection`：
`instanceReady` 表示当前代次运行实例已观察就绪；`state` 为 `preparing`、`ready`、`unknown`
或 `unavailable`；`reasonCode`、`observedAt` 和不含凭据的 `guestInstructions` 提供下一步。
VM runtime 运行事实可先于 SSH 握手入库，此时 `instanceReady=true`，SSH endpoint health
为 `unknown`，操作继续准备连接；只有真实握手取得 host key 后才发布健康 SSH 入口。
端点就绪不等于已签发 AccessGrant，授权与租约校验不变。


配置操作说明实际影响，尤其是重启、临时文件和活跃进程。默认不打断科研任务；超出预授权范围的变更需要用户确认，不能将任意用户文本作为平台管理指令执行。

停止、删除和设备释放由实际执行结果确认。失败保留明确状态和诊断，不虚报 Deleted 或可用容量。保留存储与计算资源分别处理，Environment 向 Resource 提供可信状态变化用于计量；异常清理期间费用待核实。

Access 的 `0008_funded_experiment_console_fence.sql` 将控制台能力的租约 CHECK 改为按实际资金租约而非用途分类：Work 必须带完整 lease fence；Experiment 可以保留旧的无租约形态，也可以携带完整原租约 fence。任一字段缺失或能力有效期超过 lease 到期均拒绝。Environment 的 owner/endpoint/console 解析与终端升级保留同一完整租约身份、当前有效窗及观测代次；迁移本身不创建租约、批准或资金预占。

## 验证

本地保护创建到回收、重复/乱序、取消、超时、旧 Worker 更新、跨项目访问、撤销后拒绝访问和释放失败。真实容器、KubeVirt、设备和网络行为需对应执行后端验证；本轮本地测试结果不表示完成远端部署。


## Original funded lease and ResourceApproval consumption

Existing runtime declarations and security contexts remain unchanged. This slice consumes the original baseline approval; it does not add root/capability/privileged declaration or execution support.

Environment calls Resource's authenticated `POST /internal/v1/environment-runtime-authorization`
under the existing `resource.environment.resolve` permission and Environment client
allowlist. It supplies `resourceRequestId`, optional `expectedLeaseId`, optional
`environmentId` for first-create target resolution, project/course/owner and exact
release ID/version. Resource reuses the original request→lease→claim→approval
helper and returns the original binding and lease. The verified platform revision
and canonical hash stay on that binding; this slice transports no new UID or
privileged-execution policy. The receipt expires at the earlier approval/lease
deadline. There is no second grant or runtime approval table.

Work and funded Experiment use the original Resource handoff, Environment target and lease IDs. Before provision/start/restart/reset,
including retry/recovery that creates compute, Environment reads the authority
again. Request, owner/project/course, release, canonical spec, provider and original
Environment target must agree. Native executor admission also checks the exact
runtime kind, operation and generation before a new effect. Existing baseline security
projection remains unchanged. Stop and cleanup do not require a new runtime grant.

执行器按实际副作用选择 fresh gate，而不是按计划中是否出现 Deployment/PVC 判断。
容器非空 Apply（Provision/Reset）、Scale 到正副本数（Start）和 Restart 必须重核原资金与运行授权；
空资源计划的 Scale/Restart 仍会操作已存在的计算对象，不能跳过重核。
VM Start/Restart 同样一律重核，即使计划资源列表为空；非空 Apply（Provision/Reset）也须重核。
空 Apply 不写 Kubernetes 对象，Observe 只读，Stop/Scale 到零及 Cleanup 用于停止和回收，
不要求新的运行授权。Validate、Build、Configure 不属于这两个原生执行器协议，不能以这些动作提交计算请求。
现有完整响应仍由原请求身份重放，fresh gate 失败不会把已完成效果再执行一次。

NATS executor 的准入拒绝通过 `ProviderFailureCode::RuntimeAdmission` 携带现有 `DiagnosticCode`
和重试属性；Provider 与 worker 不把它改写成通用 `LW_ENVIRONMENT_PROVIDER_REJECTED`。
只传输经过格式/长度校验的机器码，不传输 Resource 原响应、凭据或数据库错误文本；
非法诊断退为现有 `LW_ENVIRONMENT_RESOURCE_USAGE_RESPONSE_INVALID`。
例如原 lease 撤销保留 `LW_RUNTIME_LEASE_NOT_ACTIVE`，未知资金覆盖保留
`LW_RUNTIME_FUNDING_INACTIVE`；暂时不可用沿现有 operation 截止与最大尝试次数重试，
到达预算后以原诊断进入终态。

这些检查补齐执行前重核，不与 Resource 撤权原子提交。重核完成到 Kubernetes 写入之间仍有窗口，
不能把本切片视为消除了撤权竞态；跨服务撤权/实例 fencing 仍属于 #207 的后续验收。


Public `CreateEnvironmentRequest` requires `resourceRequestId`; its approved target
is resolved from the original request. Resource's normal approval → capacity claim →
funded lease → handoff creates that same Environment aggregate. The public create
binds its idempotent browser intent to the existing create operation; it creates no
second Environment, reservation, hold or grant. The release determines Work or
Experiment. A course Experiment may be funded by the student's project while the
teacher's published release belongs to the authoring project; the existing teacher,
course, actor and configuration checks still apply. Work and an unscoped Experiment
require the same project. A supplied display label must match the Resource request's
original label; omit it to use that label.

Before lease activation, a scoped and still-valid approved allocation returns
`409/LW_ENVIRONMENT_ALLOCATION_PENDING` without an Environment, operation, Outbox,
API idempotency result or hold mutation. Resource verifies the original allocation
chain and current policy before classifying it as pending; revoked/expired approvals
and wrong owner/project/course/release remain denials, not pending. After activation
and the original handoff, retry the same browser key to attach to that one create operation.

`POST /internal/v1/resource/work-handoffs` returns a `ResourceEnvironmentHandoffReceipt`:
the original `accepted` create operation plus request/lease/claim IDs and the lease
revision actually accepted by Environment. That receipt commits atomically with the
aggregate on the existing idempotency ledger. Its business fingerprint excludes mutable
request/lease/claim revisions and trace IDs; changing an immutable identity still conflicts.
A completed handoff replays before fresh runtime admission, even after permission or
funding is revoked. It reports a prior acceptance only; every new physical effect still
requires a fresh grant. Resource records the returned lease revision, never the newer
revision it merely attempted to send, then uses the original lease-sync path if needed.

If a handoff reply is lost until expiry, Resource reads
`GET /internal/v1/resource/work-handoffs/{requestId}` under the existing manage permission.
A known receipt requires real Environment cleanup and preserves the original acknowledged
fence. Only the owner's explicit `LW_ENVIRONMENT_HANDOFF_NOT_ACCEPTED` result establishes
non-acceptance; transport errors, unknown 404s and malformed replies retain capacity.
No missing acknowledgement is treated as proof that no environment exists.

Before handoff finishes, public create returns `LW_ENVIRONMENT_ALLOCATION_PENDING` (409,
retryable), and a retry uses the same intent. Identity mismatch returns 412;
idempotency/body conflict returns 409. Original Resource scope/policy errors retain
their diagnostics. Legacy Experiment aggregates without a native lease remain
readable and can stop/delete; creating new compute from them returns
`LW_RUNTIME_ALLOCATION_PATH_UNSUPPORTED` (422). No lease is attached to legacy data
and no second reservation is opened to bypass admission.

The owner and both runtime executors read the same authenticated
`GET /internal/v1/environment-credit-grants/{leaseId}` authority. The original
request/lease/claim, project, lease revision and coverage must agree with the runtime
approval receipt. Execution requires a currently covered, known Pending hold.
Unknown, Released or expired funding returns `LW_RUNTIME_FUNDING_INACTIVE` (409).
A renewal committed between the two reads returns retryable
`LW_RUNTIME_FUNDING_CHANGED` and is retried within the existing operation budget.
The execution deadline is bounded by operation budget, immutable material eligibility,
runtime approval, the original lease and credit coverage. Cleanup recovery does not
require a new compute grant or renewed material eligibility.
Renewal extends the original lease and hold; Environment refresh retains ownership
and generation. Stop preserves that lease and capacity; a stop request is not release.
Resource observes one active handed-off Environment per bounded keyset maintenance
pass, rotating the cursor. Only `Deleted` plus verified `cleanupComplete` and the exact
last acknowledged lease/claim binding begins the original release saga; stopped,
unknown, foreign and stale readbacks cannot release anything. The current Resource
lease/request/claim revisions are checked transactionally, so concurrent renewal or
revoke requires a new read. Existing completion releases the same hold once; ordinary
Delete does not wait until the original lease TTL. No new reservation or refund ledger
is introduced.
Metering uses `resource_request` for both funded purposes; the legacy
`experiment_environment` target remains only for historical unleased aggregates.

Runtime executors require the existing Resource authority client configuration:
`LABWEAVER_RESOURCE_USAGE_BASE_URI`, `_CA_PATH`, `_AUDIENCE`, `_SCOPES` and
`_TIMEOUT_MILLISECONDS`, with the shared service OIDC issuer/CA, client ID and secret
file. They use the same JWT/TLS client as the owner; no fixed principal or new runtime
grant is introduced. The #203 Bundle/profile owner must bind those existing settings
and the permitted service identity before deployment. This increment does not edit
real credentials, service allowlists or cluster permissions.

A fresh authority read happens before the executor's fencing transaction. A new
compute/storage effect also checks the current operation and generation (and the
Container plan's project). Existing completed responses can replay if current funds
are unavailable; a new effect cannot begin. Cleanup keeps its existing ownership,
UID, generation and tombstone checks and does not depend on a new credit grant.
