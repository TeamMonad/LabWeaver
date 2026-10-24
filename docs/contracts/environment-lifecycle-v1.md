# 环境生命周期

Environment 是交互环境的唯一业务生命周期。Experiment 与 Work 表达用途，Container 与 VirtualMachine 表达执行后端，二者独立组合，不为 xv6、CTF 或某门课程新增生命周期。

## 归属和创建

每个环境属于 Project，可关联 Course。Project 成员和环境所有者授权均需检查；课程结束不能删除独立科研 Work。

创建绑定明确模板、后端及资源意图。Resource 管理审批、分配与租约，Environment 在获得有效授权后创建运行资源。资源申请可以先于实际运行；一次性任务申请资源时无需创建长期 Environment。

Environment 唯一写入环境 Namespace、Quota、PVC、容器和 VM。Resource 不预建 quota shell 再让 Environment 接管。Kubernetes 对象按实例/操作代次识别，重复调用不创建第二份资源。

`POST /api/v1/environments` 只接受 Experiment release。Work 的首次创建必须通过 Resource 的
`ResourceTarget.environment` 申请、审批及租约交接；不读取项目模型配置。
已有 Work 的 `start`、`restart`、`retry`、`recover`、`reset` 重新校验原租约的当前 Resource
授权和容量绑定。原租约有效时直接操作；失效、撤销或释放后明确拒绝，不能靠旧的 Environment
快照恢复运行。首次申请之外不强制重新人工审批；是否需要新申请取决于 Resource 当前授权。
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
