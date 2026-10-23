# Kueue 抢占事件与现有执行 API 对照（已废止）

状态：**Superseded by [ADR 0018](../adr/0018-kueue-admission-and-preemption-boundary.md)**。

本文件曾是 Issue #211 的研究提案，不再是 LabWeaver 的实现契约。不得从本文读取版本、Workload owner、字段 writer、60 秒保存或 VM 接入语义；这些旧结论已失效。

当前有效约束：

- Kueue 固定为 v0.20.2，写入 API/CRD 为 `kueue.x-k8s.io/v1beta2`。
- Kueue native integration 唯一管理 Workload；Resource 不创建、patch 或删除 Workload，只持久化业务 approval、source UID、Workload UID 和 binding revision。
- Deployment 使用逐 Pod plain-Pod integration；一次性任务使用 `batch/v1 Job` integration；Environment/task-execution 管 source object，Kueue 管 Workload/admission/eviction。
- 可抢占有状态 Work 采用 Kueue 原生立即 eviction。只保证 SIGTERM 与管理员配置的 `terminationGracePeriodSeconds`（默认 30 秒）；只保证已落盘/提交文件，不保存进程内存。
- Environment 必须对账被动 eviction 和重建 UID；新 UID 在匹配 Resource binding、Environment generation 和 Kueue admission 前保持 `SchedulingGated`。Resource 在 attachment/storage fencing 未确认前不得 Released、不得扩大 quota、不得重新借出容量。
- 5 分钟窗口从源 Pod API `metadata.deletionTimestamp` 起算，到新 generation 完成 Resource/Kueue admission、运行和可连接；超时退款不可用期间费用并追加管理员配置固定补偿，使用幂等 ledger event，且不改变未确认的 allocation/fencing 状态。
- VM 不接入 VMI pod-integration；VM 使用 Resource-only + 共享 reservation/fencing projection。GPU/MIG/vGPU 依赖 #213/#214，真实验证前禁止共享容量上线。

实现和测试请只引用 ADR 0018、Sprint5 计划及其后续实现 Issue；本文件保留用于历史追溯。
