# ADR 0016：一次性任务资源准入与 Kubernetes 执行边界

状态：Issue #192 的实施契约；实际实现与测试见该 Issue 的 PR。人类架构/安全 Review 由 A+B 完成，行为 Verify 由 D 完成。

## 背景

Agent authoring、Evaluation runner、Build/Work 配置与未来任务生产者都需要一次性 Kubernetes 工作负载。如果每个业务域自行创建 Job、自行解释 Resource 租约并自行处理取消、恢复与清理，就会出现平行资源准入、语义漂移和僵尸 workload。与此同时，Environment 已拥有长生命周期 Container/VM，Resource 已拥有批准、CapacityClaim、Lease、Quota 与费用；平台不需要第七个业务服务或新的 Scheduler。

本决定冻结四层职责，使 #191 的 Agent sandbox 与 Evaluation runner 迁移复用同一 Resource/Execution 边界，而不是各自形成第二套执行体系。

## 决定

### 四层职责

| 层 | 回答的问题 | 权威 |
| --- | --- | --- |
| Business（Evaluation/Agent/Work 配置） | 为什么执行、输入是什么、何时算成功、结果如何解释 | Task/Run 状态与业务结果 |
| Resource | 这个 TaskRun 是否允许占用资源，允许多少、多久、哪个 Provider | ResourceRequest/Approval、CapacityClaim、ResourceLease、Quota、用量与费用 |
| Execution | 在已取得资源授权的前提下，如何创建、观察、取消并确认清理 Kubernetes workload | 工作负载身份、观察、取消与清理结果 |
| Kubernetes Scheduler | Pod 到节点的实际调度 | kube-scheduler |

业务服务不得通过 Kubernetes 可用容量推断资源已批准。`CapacityProvider` 只负责容量供给与映射，不承担业务执行语义：`CapacityProvider != ExecutionBackend`。

### 资源领域契约保持不变

`ResourceTarget::Task { task_run_id }`、`ResourceRequest`、`CapacityClaim`、`ResourceLease` 继续是唯一资源领域契约。不引入 `ResourceClaim`、`JobRequest` 或第二套 Lease。一个执行代次绑定唯一 `TaskRunId`；同一执行代次恢复时复用原申请与租约；新执行代次使用新 `TaskRunId`，因此不得复用已释放的租约。Resource 释放仍以 claim/lease revision 条件更新实现 fencing。

### Execution 契约

`crates/contracts` 持有与基础设施无关的执行身份与结果数据契约：`TaskExecutionBinding`、`ExecutionObservation`、`ExecutionWorkloadState`、`ExecutionObjectRef`、`ExecutionCleanupStatus`。绑定至少包含 `TaskRunId`、execution generation、ResourceRequestId、CapacityClaimId、LeaseId + revision、project、provider binding、namespace、workload identity 与 trace id。

绑定只能由已 Active 的请求、已 HandedOff 的 claim、已 Active 的 lease 与一致 namespace/revision 的权威 `TaskResourceStatus` 构造（`from_admitted_status`），并在业务服务的恢复检查点中持久化。无有效 Active/Admitted 绑定不得创建 workload：新执行只能经 acknowledge 成功后的绑定进入 submit；恢复必须重新读取 Resource 状态并确认同一 reservation，revision 因续租推进时只允许更新 fence，不改变 reservation 身份。

执行机制只包含 `submit` / `observe` / `cancel` / `cleanup`，不持有成绩、成功判定或 Agent 业务语义，也不得审批资源、创建 Environment、自动修改 Resource 请求或绕过准入。

### Kubernetes Execution Backend

Evaluation 域内的统一 backend 负责 Job 创建、namespace/workload identity、resource requests/limits、deadline、cancellation、Job/Pod 观察、terminal 机制结果、确定性清理、duplicate submit 幂等、Worker crash/recovery、stale generation fencing 与 orphan workload detection。backend 不审批资源。

只有出现至少三个共享相同 Kubernetes 执行语义的正式生产调用方时才抽取独立 crate；本决定生效时唯一生产调用方是 Evaluation，因此契约在 `crates/contracts`，实现保留在 Evaluation 内。不建立通用 `utils` 层。

### 清理与释放顺序

业务结果先持久化，再清理执行对象，最后释放 Resource。`cleanup` 返回 `Confirmed`、`Pending` 或 `Unknown`；只有 `Confirmed` 才能确认释放，`Unknown` 不得改写为 Released。Job 消失、Worker 失联、HTTP 请求成功或删除请求发出都不表示资源已释放。已持久化 terminal result 的尝试在 Job 已删除时恢复只做清理确认，不重新执行。

### 孤儿 workload

Evaluation worker 周期性按 `labweaver.io/managed-by=evaluation-service` 标签列出执行对象，与持久 attempt/checkpoint 比对。只有当数据库记录证明对象属于已终态或已放弃的尝试，且标签、request SHA-256 与 UID 归属一致时，才按 UID 前置条件自动清理并验证消失；归属不确定时只输出稳定诊断，不删除。

### Kueue 与未来扩展

Execution/Resource 契约保留 workload class、priority、preemptible、queue/admission binding、accelerator class 与 topology requirement 的扩展点。未来若引入 Kueue，另建独立 Issue，由 Resource 将业务 Resource Policy 映射到 Kubernetes/Kueue admission；业务服务仍不得直接理解 `ClusterQueue`、`LocalQueue`、`ResourceFlavor` 等基础设施概念。Kueue 是后续 admission backend，不是 LabWeaver 业务 Scheduler。

## 状态与竞态

必须保持确定性并输出稳定 diagnostic 的失败：审批未完成、批准失效、claim Blocked、lease revision 不匹配、duplicate submit、submit 后 Worker 崩溃、清理失败、Job 不存在但本地仍 Running、cancel 与 completion 竞态、lease expiry 与 completion 竞态、stale generation 写入、Kubernetes API 暂时不可用、orphan 重现、result 已持久化但 cleanup 未完成、release 重复投递。失败不得转换为成功或学生/用户错误。

## 代价与边界

- Resource 不新增 execution generation 字段；generation fencing 由业务检查点中的绑定与 TaskRunId 唯一性承担。这是有意的简化：Resource 只认 claim/lease revision，业务恢复语义由 Evaluation 维护。
- 统一 backend 暂不覆盖 Freeze 提交 Job：该路径没有 Resource 租约，语义不同，留在既有 coordinator。
- 本决定不改变 Environment 对长生命周期 Container/VM 的所有权，也不把 Environment 转换为 Job。
- 不引入 Kueue、Volcano、自研 Pod-to-Node 调度、真实云扩容、Slurm 或多集群调度。

## 验证边界

本轮只做代码与本地验证：PostgreSQL 使用 testcontainers，Kubernetes API 在业务链测试中使用进程内 TLS mock；执行 backend 的真实 Job 创建/观察/取消/清理/readback 在本地 kind 集群验证。不执行远端部署、不操作共享集群、不真实扣款。真实 GPU/KubeVirt 能力仍需对应硬件验证。
