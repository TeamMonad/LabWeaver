# 环境生命周期

Environment 是交互环境的唯一业务生命周期。Experiment 与 Work 表达用途，Container 与 VirtualMachine 表达执行后端，二者独立组合，不为 xv6、CTF 或某门课程新增生命周期。

## 归属和创建

每个环境属于 Project，可关联 Course。Project 成员和环境所有者授权均需检查；课程结束不能删除独立科研 Work。

创建绑定明确模板、后端及资源意图。Resource 管理审批、分配与租约，Environment 在获得有效授权后创建运行资源。资源申请可以先于实际运行；一次性任务申请资源时无需创建长期 Environment。

默认模式由 Environment 唯一写入环境 Namespace、Quota、PVC、容器和 VM。既有 namespace 模式由部署管理员保留 namespace 及其整体策略的管理权，Environment 只管理本 Work 对象。Resource 不预建 quota shell 再让 Environment 接管。Kubernetes 对象按实例/操作代次识别，重复调用不创建第二份资源。

## 既有 namespace 运行落点

本增量支持一个部署指定项目的普通 Container Work。它仍通过 Resource 授权、lease 和 Environment 生命周期创建；客户端不能选择 namespace。VM 和 Experiment 不使用此模式。普通容器的 UID、capabilities 和 rootfs 声明沿下文同一运行授权执行，网络声明须为 `deny_all`。既有 namespace 的 PSA、配额和网络策略由部署管理员提供，Environment 不覆盖或绕过它们；配置不被该空间准入时明确失败。本模式不提供 privileged、持久盘独立回收、7天保留或导入导出。Namespace 不是宿主隔离或特权运行边界。

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

兼容的 Container Work 升级恢复为 HTTP 202，沿原 durable Restart 执行。入口在 Access 撤权前
读取 Resource 的 fresh 授权和资金覆盖；Resource 从 Control 发布事实验证目标已发布、未撤回、
与原批准同族，且权限投影相等。权限投影共用 `EnvironmentSpec::permission_projection()`，包括
class、全部 resources/network/security/entries，以及 Container provider 和 service port；
build context、镜像内容、终端程序、名称和材料保留期限不是新增权限。VM 不开放此升级路径。
资源规格的任何变化（包括降低）仍需新申请，不会静默降配。

升级不修改原批准和 request target，不新增 request、approval、claim、lease、reservation 或 hold。
后台 Restart 和 executor 每次新计算前重新核对原资金覆盖与目标 release 的权限投影。
原批准保留原 release/content SHA 的审计身份，不能驱动更宽的新配置。
同一 Idempotency-Key（包括并发和响应丢失重放）只受理一次 operation。
`operation.templateUpgrade` 保存 source/target release 和 `dataPolicy=preserve_workspace`；
operation 状态可查询。失败不会伪装成功，`/workspace` 的原 PVC 保留；
重放原升级请求返回原 operation，显式 recover/retry 沿现有生命周期恢复已确认的目标。
容器临时根文件系统和进程内存不保留；升级不追加资金预占，实际用量仍由原 lease 计量。

旧批准缺少 `permissionProjectionSha256` 时返回
`409/LW_RUNTIME_AUTHORIZATION_REAPPROVAL_REQUIRED`，中文提示重新提交资源申请。
不回填或覆盖历史批准，不新增历史投影表；旧批准不能执行升级、Restart 或其他新计算。
停止/清理不被此检查阻塞。v1 清空重建仅可在维护者另行授权的切换窗口执行，
代码不依赖清空已经发生，本变更不执行数据清空。

Access 撤权仍走生产 `NatsAccessRevoker`，双方使用 contracts 中共享的
`AccessRevocationRequest/Response` 与受限 reason 枚举；回复的版本、environment ID/revision
必须精确匹配。撤权响应错误或超时不会受理升级，不写目标版本或 operation，
用户查询仍得到原状态，可使用同一幂等键重试；资金 hold 不变。


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


## 普通容器运行权限

`EnvironmentSpec.security` 分别声明容器执行身份、能力与安全开关。同一不可变 release 的规范化规格摘要
绑定全部字段，消费者从这份声明与匹配的 verified receipt 投影，不提供另一套兼容权限路径：

| 字段 | 默认与当前执行边界 |
| --- | --- |
| `runAsUser` | `null` 消费同一已验证平台版本的 `defaultContainerUid`；平台默认值为 `0`。显式 `0` 表示容器 root，`1..4294967294` 原样使用；保留 UID `4294967295` 被拒绝。 |
| `privileged` | 默认 `false`；当前 `true` 明确拒绝。root 不等于 privileged，也不开放 DinD。 |
| `capabilities` | 默认空集合；始终 `drop: [ALL]`，只把本版本实际声明且获准的能力加入 `add`。平台允许清单是上限，不是自动授予实例的最大权限。 |
| `allowPrivilegeEscalation` | 仅支持 `false` 或 `null`；缺省及 `null` 归一化为 `false`。`true` 在唯一 Rust spec 语义校验中拒绝，Provider 不能静默改为 false 执行。root、capabilities 和 rootfs 声明均不能绕过此限制。 |
| `readOnlyRootFilesystem` | 默认 `false`，允许容器系统目录临时可写；显式 `true` 投影为只读 rootfs，不等于整个 Work 磁盘只读保留。 |
| `seccomp` | 默认且当前仅支持 `runtime_default`；`unconfined` 被拒绝。 |

普通能力只接受 `LinuxCapability::supported_in_container` 当前支持的 Linux UAPI 名称，例如
`NET_ADMIN`、`NET_RAW`、`CHOWN`。`ALL`、未知名称及 `SYS_ADMIN`、`SYS_MODULE`、`BPF`、
`PERFMON` 等更广的内核权限不开放。声明仍须满足平台 `allowedCapabilities`、当前对象范围及适用的
原教师批准/项目或课程预授权；更改运行权限要形成新的配置版本并重新核对，不能扩大旧批准。

运行授权 receipt 带上同一 verified applied 版本的 `defaultContainerUid`。消费者从不可变 spec 与该
receipt 唯一计算实际 UID、安全上下文及规格身份；不能从另一份 desired 或最新配置拼入默认 UID。
创建、启动、重启、重置及会产生计算的恢复都沿原 Resource 申请、审批、lease 和资金覆盖重核。
平台换版后旧 receipt 不会自动升级成新权限。`securityProfileBinding` 和禁止公开暴露的约束保持生效。

Environment 自己创建的运行 Namespace 使用 PSA `enforce=baseline`（能力集合为空）或
`enforce=privileged`（能力集合非空），`audit/warn=restricted`。后者仅使该运行空间能够准入
获准的能力，例如 baseline 不接受的 `NET_ADMIN`；Pod 仍为 `privileged=false`，仍 drop ALL 后只 add
获准集合。这个 PSA 标签不是特权执行授权，不改变平台、Evaluation 或 builder 所在空间，也不给用户
或运行 Pod Kubernetes API 权限。运行 ServiceAccount 不自动挂载 token，不增补 RBAC；宿主 namespace、
hostPath、平台凭据和额外设备不会因 root/capabilities 自动开放。既有 namespace 模式不改管理员的 PSA。

普通容器的所有带对象 Apply 和会创建或变更计算的动作，持久 executor 都从可信不可变 spec/release、
同一 verified receipt 和部署配置调用现有唯一投影，重建该动作的完整可信计划。接收列表必须与它的
**对象数量、顺序和内容全部相同**；按身份逐对象比较不能接受有序子集，也不保留携带计算对象的稀疏例外。
即使 Pod/Deployment 本身的 securityContext 完全相同，遗漏 NetworkPolicy、ResourceQuota、LimitRange
或其他应有对象也必须拒绝，不能把隔离策略和资源上限视为可选附带对象。

executor 使用与可信投影相同的 canonical 摘要函数、相同输入结构重算接收对象列表的 SHA，再与可信计划
SHA 比较。复制可信 planSha256 却删改列表会失败；为被裁剪、重排或修改的列表自算一个新 SHA 也不能
取得执行权。调用方声明的摘要不是完整性事实，更不能代替数量、顺序和逐对象内容一致性检查。

完整安全面包括 Pod/Deployment 的命令和参数、环境、工作目录、安全上下文、init/sidecar、ServiceAccount
与 token 挂载、卷及挂载、宿主 namespace/网络设置、Namespace PSA，以及隔离策略、配额和 LimitRange。
未声明字段、额外对象、缺少对象、对象重排或任何漂移都会在 Kubernetes 副作用前拒绝。合法计算路径使用
完整计划；测试中的稀疏消息只用于无授权攻击负向场景，不能作为生产计算路径的兼容入口。
只读观察、纯停止/回收仍沿其各自协议与 fence，不因此取得新运行权限；这些路径不构成上述 Apply 与计算动作的稀疏计划豁免。

`allowPrivilegeEscalation=true` 的拒绝由 Rust spec 语义校验这一真源完成；显式 `false` 和输入 `null`
得到同一规范化 false 投影。Provider/Executor 不能把不支持的 true 静默钳制为 false，也不能通过相同
securityContext 放行带有 hostPath、额外 init container、宿主 namespace 或更宽 PSA 的其他对象字段。

Provider 与 container executor 共用 `ContainerProviderConfiguration.project_plan`，不另建一份权限表或简化
投影。Helm 仅为 container-executor 添加 `container-projection` 只读卷，来源是 environment-service 已有
ConfigMap 的 `providers.json`；目录挂载为 `/etc/labweaver/container-projection`，只投影该 key。
模板独占 `LABWEAVER_ENVIRONMENT_PROVIDER_BINDINGS_PATH=/etc/labweaver/container-projection/providers.json`，
拒绝 workload env/secretEnv 覆盖（即使 env 重复填写相同路径也不接受）。该文件提供同一部署输入，
不是第二份 profile、策略或授权，不能替代原 spec、receipt、资金覆盖与 Kubernetes 对象归属检查。

普通容器 rootfs 的安装和系统修改仍是临时层，`/workspace` 持久盘与其分开；需要保存软件环境时走
现有模板/镜像构建流程，不把停止后保盘理解为保留系统层。权限声明也不会增加 builder 的 capabilities、
提权或 entitlements。

VM 此切片保持原 guest 用户、SSH 和持久系统盘行为：不能用 `runAsUser` 指定 guest UID，不能添加容器
capabilities、启用容器提权或声明容器只读 rootfs；不支持的组合直接拒绝，不伪装成 guest 能力已经执行。
当前没有 privileged、DinD、宿主隔离/fencing 或回收站验收结果，不能从投影字段或 PSA 标签推定这些能力。

## Original funded lease and ResourceApproval consumption

The original approval now binds the complete ordinary-container security declaration described above. The same verified receipt supplies the default container UID; no second grant or independent permission state is created. VM guest execution and privileged/DinD support are not expanded by this slice.

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
again. Request, owner/project/course, provider and original Environment target must
agree. Experiment and VM retain exact release/content identity; compatible Container
Work uses the same-family permission projection described above. Native executor admission checks the exact
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
