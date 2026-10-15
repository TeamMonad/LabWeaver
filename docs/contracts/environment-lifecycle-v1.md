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

停止只退役本 Work Pod。删除先清除本 Work 的计算对象，再清除 PVC。每次对象删除都核固定名称和归属，并携带 UID/resourceVersion 前置条件。计算阶段结束前，Executor 读取完整的 namespace Pod 列表，确认本 Work 的 Pod，以及引用本 Work PVC 的 Pod 均已消失；缺少或不同归属标签的挂载 Pod 也会阻止盘清除。Pending 或 terminating Pod 仍视为存在。列表不完整、格式未知或超时均不进入盘清除，重试重新观测，不保存可复用的“已经卸载”标记。

清理不删除既有 Namespace，不移除任何 finalizer，也不删除其他 Work、部署策略或冻结 Job。未知归属、UID 变化和超时保持失败状态。Pod 消失的观测不替代存储后端的挂载 fencing。删除仍会删除本 Work PVC，七天回收站、恢复和独立磁盘清除终态尚未交付。删除操作证据区分 `namespaceRetained=true`、`workObjectsAbsent=true` 和默认模式的 `namespaceAbsent=true`。

回退此增量前，先通过当前版本正常收尾既有 namespace Work。不要切回默认模式来迁移现有 Work，也不要删除其落点账本记录。真实定位和凭据保留在私有部署输入；Issue/PR 只记录合成验证结果。

`POST /api/v1/environments` 只接受 Experiment release。Work 的首次创建必须通过 Resource 的
`ResourceTarget.environment` 申请、审批及租约交接；不读取项目模型配置。
已有 Work 的 `start`、`restart`、`retry`、`recover`、`reset` 重新校验原租约的当前 Resource
授权和容量绑定。原租约有效时直接操作；失效、撤销或释放后明确拒绝，不能靠旧的 Environment
快照恢复运行。首次申请之外不强制重新人工审批；是否需要新申请取决于 Resource 当前授权。
HTTP 与 JetStream 生命周期入口对 Work Restart 使用同一当前租约验证；过期或撤销的授权不进入命令事务，重复事件沿原 Inbox/幂等账本处理，不产生第二个操作。
租约尚未激活、非 Active、Active 但 Resource 返回 `authorization:null`，以及结构合法但未到激活时间/已经过期的授权快照，均是业务拒绝：HTTP `409/LW_RUNTIME_LEASE_NOT_ACTIVE`，不是上游协议损坏。Resource 会对已过期但数据库状态仍为 Active 的租约返回 Active + null，调用方不得将其误报为 503。JetStream 入口沿用同一业务诊断拒绝，不写命令事务。

只有反序列化失败、错误版本、授权身份/范围不匹配或授权结构非法等协议错误才返回 `503/LW_ENVIRONMENT_LEASE_VERIFICATION_RESPONSE_INVALID`，NATS 隔离记录也保留此诊断。上述映射覆盖所有 Work verifier 调用方：启动、重启、重试、恢复、重置、模板升级、Resource handoff 和租约刷新。
费用估算复用已生成的 `estimateProjectResourceCost`，输入 `resources`、`computeFrom/computeUntil` 与 `storageFrom/storageUntil`，返回 `ResourceCostEstimate` 的固定费率明细和 `total`；不由 Environment 或浏览器另建计价逻辑。该接口不依赖模型，也不授予容量或资金权限，见[计算与存储估价](project-credits.md#计算与存储估价)。


### 显式升级 Work 模板

`POST /api/v1/environments/{environmentId}/template-upgrade`（SDK `upgradeEnvironmentTemplate`）
要求 `Idempotency-Key`、当前 `If-Match` 和 `{targetReleaseId,targetReleaseVersion,confirmed:true}`。
仅容器 Work 可接受同项目、同课程、同模板族且版本更高的 release；资源、网络、安全策略、
provider、端口和入口必须与原批准配置一致，否则以
`LW_WORK_TEMPLATE_UPGRADE_REQUIRES_RESOURCE_APPROVAL` 拒绝。VM 总是以 HTTP 422
`LW_WORK_TEMPLATE_UPGRADE_UNSUPPORTED_FOR_VM` 拒绝，并返回中文 `ProblemDetails.detail`；不隐式重建。

服务端先验证兼容性，再校验当前 Resource 租约和撤销旧连接。接受事务只改变期望 release，
复用原 Restart、Reconciler、代次、Outbox 与操作查询，不新建资源申请或第二套升级状态机。
升级沿用 `environment_restarted` 撤销原因，不引入对端不支持的新 reason。必须收到 Access 对同一实例/版本的确认后才进入升级事务；撤销未确认时返回明确失败，原 release、操作和 PVC 绑定不变。
`getEnvironmentOperation` 和历史列表中的 `templateUpgrade` 保存源/目标 release 及
`dataPolicy: preserve_workspace`；状态与错误直接来自同一 operation。`releaseId/releaseVersion`
此时是已确认的期望目标，不能独立于 `state` 当作升级完成。Deployment 的新副本、就绪副本和
总副本都收敛后才完成；旧 Pod 仍可用不能令升级成功。

Namespace、`workspace` PVC、Resource 申请、租约、批准资源及原数据保留期限不变；仅
`/workspace` 持久数据保留。容器临时 rootfs 与进程会随重启丢失。失败/超时进入 Failed 并
保留数据，不进入删除 namespace 的超时清理；取消升级走停止实例而不是删除 PVC。
用户重试或恢复继续同一个目标 release；不做未经确认的镜像回滚。

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

## 验证

本地保护创建到回收、重复/乱序、取消、超时、旧 Worker 更新、跨项目访问、撤销后拒绝访问和释放失败。真实容器、KubeVirt、设备和网络行为需对应执行后端验证；本轮本地测试结果不表示完成远端部署。
