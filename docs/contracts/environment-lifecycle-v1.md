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
2. 将该文件放入一个部署管理员维护的 ConfigMap 的 `profile.json` key。设置 Helm 的 `containerPlacementProfileConfigMap` 为该对象名。模板只把同一对象只读挂载到 Environment owner 和 Container executor，并设置相同文件路径；不会创建该 ConfigMap 或目标 Namespace。
3. 非 Helm 启动同样使用绝对文件路径，并给两个进程提供同一 profile。文件缺失、未知字段、非法名称、零 UID、非 Work/指定项目或不支持的网络声明明确失败。
4. 部署管理员在批准的操作范围内检查 namespace 现行 PSA、资源预算、存储类和网络隔离是否适合本 Work 及冻结 Job。Environment 不创建或覆盖 namespace 的 PSA、quota、LimitRange 或 NetworkPolicy，也不扩大 RBAC。现行 `deny_all` 依赖该专用 namespace 的部署策略；本模式不能用于未隔离的多租户共享 namespace。

每个 Work 使用 `lw-{environmentId}-runtime` 的 Deployment、Service 和 ServiceAccount，`lw-{environmentId}-workspace` 的 PVC，以及 `lw-{environmentId}-registry-pull` 的拉取 Secret。容器内名称和工作目录仍为 `runtime` 和 `/workspace`。CPU/内存/磁盘申请仍来自原 Resource 授权；模式不增加资源预算或授权范围。

Executor 在首次 Kubernetes 副作用前，与现有代次 fence 同一事务将 namespace 名称、UID 和 Work/项目身份保存为 Environment idempotency ledger 的不可变结果。重启和重放读取原结果。改变配置不能迁移原 Work，namespace 同名重建也被 UID 检查拒绝。旧版本已执行的专用 namespace Work 不能被直接采用为既有 namespace Work。无需新表或迁移；该账本结果必须与 Environment 事实一起保留。

Freeze binding 从该事实返回真实 namespace/PVC。终端、Work 配置脚本和 Access 应用代理也消费同一落点；应用代理从现行内部 endpoint eligibility 获取 `runtimeTarget`。默认模式仍返回原专用 namespace 的目标。Owner、项目、revision、Ready、观测代次、lease 和端点健康资格检查保持生效。Evaluation coordinator 由 #197 owner 组合，不能改写 binding 或跳过资格检查。

停止只退役本 Work Pod。删除按固定对象名逐一核归属，并携带 UID/resourceVersion 前置条件；Deployment 前台回收完成后才删除 PVC。清理不删除既有 Namespace，不移除任何 finalizer，也不删除其他 Work、部署策略或冻结 Job。未知归属、UID 变化和超时保持失败状态。删除仍会删除本 Work PVC，数据回收站尚未交付。删除操作证据区分 `namespaceRetained=true`、`workObjectsAbsent=true` 和默认模式的 `namespaceAbsent=true`。

回退此增量前，先通过当前版本正常收尾既有 namespace Work。不要切回默认模式来迁移现有 Work，也不要删除其落点账本记录。真实定位和凭据保留在私有部署输入；Issue/PR 只记录合成验证结果。

## 状态、访问和回收

desiredState 表达请求，observedState 表达实际状态。请求受理不等于 Ready，停止请求不等于已释放。保留现有 Reconciler、持久任务、取消、租约、Outbox 和条件更新，不复制第二套状态机。

Web Terminal、noVNC、SSH/SFTP 连接使用相同 AccessGrant 边界。实例状态、端点健康、项目授权和租约均有效时才能建立新连接；撤销、到期、停止和删除触发服务端连接清理。失败不得靠页面隐藏按钮代替服务端拒绝。

配置操作说明实际影响，尤其是重启、临时文件和活跃进程。默认不打断科研任务；超出预授权范围的变更需要用户确认，不能将任意用户文本作为平台管理指令执行。

停止、删除和设备释放由实际执行结果确认。失败保留明确状态和诊断，不虚报 Deleted 或可用容量。保留存储与计算资源分别处理，Environment 向 Resource 提供可信状态变化用于计量；异常清理期间费用待核实。

## 验证

本地保护创建到回收、重复/乱序、取消、超时、旧 Worker 更新、跨项目访问、撤销后拒绝访问和释放失败。真实容器、KubeVirt、设备和网络行为需对应执行后端验证；本轮本地测试结果不表示完成远端部署。


## Runtime declaration and ResourceApproval consumption

`EnvironmentSpec.security` is the only runtime declaration. `runAsUser` is nullable:
null consumes the same verified platform version's `defaultContainerUid` for a
container (default 0); VM guest defaults to 0. Explicit nonzero UIDs are preserved.
`privileged` defaults to false, `capabilities` to an empty closed `LinuxCapability`
set, `seccomp` to `runtime_default`, root filesystem to `mutable_required`, and
privilege escalation to `deny`. Old `userPolicy` is rejected. UID/root filesystem,
capabilities, escalation and seccomp are all covered by the original canonical
configuration hash. Runtime rights never authorize the image builder.

Environment calls Resource's authenticated `POST /internal/v1/environment-runtime-authorization`
under the existing `resource.environment.resolve` permission and Environment client
allowlist. It supplies `resourceRequestId`, optional `expectedLeaseId`, optional
`environmentId` for first-create target resolution, project/course/owner and exact
release ID/version. Resource reuses the original request→lease→claim→approval
helper and returns that binding, its lease and values from the same verified
platform version. The receipt expires at the earlier approval/lease deadline.
There is no second grant, runtime approval table or independently configurable UID.

Work and funded Experiment use the original Resource handoff, Environment target and lease IDs. Before provision/start/restart/reset,
including retry/recovery that creates compute, Environment reads the authority
again. Request, owner/project/course, release, canonical spec, provider and original
Environment target must agree. Provider and executor admission derive the same
security context; executor compares the rendered context with the durable instance
and immutable release. Stop and cleanup do not require a new runtime grant.

The current container executor supports the Kubernetes baseline capability additions
plus `NET_ADMIN` and `NET_RAW`, only within the verified project's allowed/preauthorized
range. Broader kernel capabilities and actual `privileged` execution are explicitly
rejected until a verified execution boundary is implemented. A selector or namespace
alone is insufficient. `SYS_ADMIN` also requires explicit escalation permission in
the declaration ([Kubernetes SecurityContext](https://kubernetes.io/docs/tasks/configure-pod-container/security-context/)).
Existing namespace PSA/quota/network policies remain operator-owned: incompatible
root/capability requests can be rejected by cluster admission, without rewriting
those policies ([Pod Security Standards](https://kubernetes.io/docs/concepts/security/pod-security-standards/)).

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

Before handoff finishes, public create returns `LW_RUNTIME_ALLOCATION_PENDING` (409,
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
The execution deadline is bounded by runtime approval and credit coverage.
Renewal extends the original lease and hold; Environment refresh retains ownership
and generation. Stop/delete retain that lease through the original cleanup handshake.
Metering uses `resource_request` for both funded purposes; the legacy
`experiment_environment` target remains only for historical unleased aggregates.

Runtime executors require the existing Resource authority client configuration:
`LABWEAVER_RESOURCE_USAGE_BASE_URI`, `_CA_PATH`, `_AUDIENCE`, `_SCOPES` and
`_TIMEOUT_MILLISECONDS`, with the shared service OIDC issuer/CA, client ID and secret
file. They use the same JWT/TLS client as the owner; no fixed principal or new runtime
grant is introduced. The authenticated Bundle opening path now derives both executors' DB,
Resource/OIDC/client and NATS endpoint/CA inputs from the existing Environment
owner's source. Each role retains its own NATS certificate/key/credentials and
provider subject. Helm mounts only the selected owner keys read-only; conflicting
executor overrides are rejected. Startup accepts exactly one of NATS `server` or
absolute `serverFile`, including the shared projected endpoint. The same placement
profile still mounts to the Environment owner and Container executor.

These bindings do not verify a real Resource allowlist, identity, installed cluster
or capacity. External-profile client/scopes/CA and the approved operation window
still require their existing owner. This increment does not edit real credentials,
service allowlists or cluster permissions.

A fresh authority read happens before the executor's fencing transaction. A new
compute/storage effect also checks the current operation and generation (and the
Container plan's project). Existing completed responses can replay if current funds
are unavailable; a new effect cannot begin. Cleanup keeps its existing ownership,
UID, generation and tombstone checks and does not depend on a new credit grant.
