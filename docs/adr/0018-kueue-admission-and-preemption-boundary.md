# ADR 0018：Kueue 准入、交互 Work 与可回收借用边界

状态：Issue #211 的实施契约；本 ADR 锁定 Kueue API 与对象所有权，准入实现和业务账务实现分别属于 #212 及后续 Issue。人类架构/安全 Review 由 A+B 完成；本 ADR 的技术证据见末尾 PoC 记录。

日期：2026-10-22

## 背景

Sprint5 同时提供有保证容量的预留和可回收借用的可抢占 Work。Resource 已经拥有商业资格、审批、CapacityClaim、Lease、配额、用量和费用；Environment 已经拥有长生命周期 Work 的运行代次、实例、持久盘和端点。Kueue 只应承担 Kubernetes 层的队列配额、公平性、准入和 Kueue 语义的驱逐，不能成为第二个 Resource，也不能成为 Environment 的第二个运行对象控制器。

Kubernetes 同一资源只能有一个管理者。Kueue 的 Pod integration 会为 Pod 注入 scheduling gate，并在 Workload admitted 后移除 gate；原生抢占会终止并删除被抢占的 Pod，Environment 必须对账被动删除和重建 UID。Kueue 不得管理 VM、磁盘或 `virt-launcher`，Resource 不能在 fencing 未确认前把 Kueue eviction 当作商业容量释放。

## 决定

### 1. 固定版本和支持矩阵

- 生产固定 **Kueue v0.20.2**。
- Kueue 固定使用 **`kueue.x-k8s.io/v1beta2`** CRD/API（包括 `Workload`、`ResourceFlavor`、`ClusterQueue`、`LocalQueue`、`WorkloadPriorityClass`）。v1beta1 即使在 CRD 中作为兼容的 served 版本存在，也不是 LabWeaver 写入版本。
- Kueue v0.20.2 的官方安装说明要求 Kubernetes **1.34 或更新版本**，其 v0.20.2 发布说明列出 Kubernetes 1.35 baseline/extended Kind 验证。因此它覆盖 v1 的 Kubernetes **1.35.8**；本地 PoC 使用等价的 Kind Kubernetes **v1.35.0** 节点镜像（API client 为 v1.35.8），不把 Kind 1.35.0 的结果冒充 v1.35.8 集群验收。
- 本分支基线的 `deploy/versions.lock.yml` 仍显示 `kubernetes.patch: v1.35.6`；这与 Kueue 的支持下限和 1.35 API 线兼容，但不是 v1.35.8 的部署锁变更。v1 发布锁若已由部署切片推进到 1.35.8，应由部署 Issue 更新锁文件，不在本 ADR 偷换锁值。
- Kueue 安装使用发布资产 `https://github.com/kubernetes-sigs/kueue/releases/download/v0.20.2/manifests.yaml`，并在部署前核对 CRD 的 storage/served 版本和镜像摘要。生产部署不得使用 floating `latest`。

### 2. 唯一管理者和对象边界

| 事实或字段 | 唯一 writer | Kueue/其他组件边界 |
| --- | --- | --- |
| ResourceRequest、Approval、CapacityClaim、ResourceLease、商业资格、售出预留、Credits、费用、退款和补偿 | **Resource** | Kueue 的 admission 不代表商业批准、售出容量、账务结算或 Resource lease release。 |
| 召回通知、eviction 时间、重启截止、运行代次和 Resource fencing | **Resource**（持久状态；调用 Environment 执行） | Kueue 负责原生 eviction；不能自己启动业务计时、确认 fencing、退款或补偿。 |
| Work/Environment 的 desired revision、运行 plan、Deployment、VM、PVC/磁盘挂载、端点和运行代次 | **Environment**（Environment executor 代表 Environment 写 Kubernetes 对象） | Kueue 不能写 Deployment/VM/PVC 的业务字段，也不能创建 Environment 的第二份实例。 |
| Kueue `Workload` object, `spec`/`status`, queue, PodSet, priority, admission, `active` and eviction | **Kueue native integration/controller** | Resource projection MUST NOT create, patch, delete, or set an ownerReference on a Workload. Kueue creates the Workload from the source Pod/Job and owns its UID, spec and status. |
| Pod 到节点的绑定和实际放置 | **kube-scheduler** | LabWeaver 不增加 Pod-to-Node scheduler。ResourceFlavor 只表达受 Resource 认可的拓扑/设备类别。 |
| VMI、`virt-launcher` Pod 及其 guest 生命周期 | **KubeVirt controllers**，由 Environment 通过官方 VM 生命周期 API 请求 | LabWeaver 不 patch 或 delete `virt-launcher`，不把 VMI 当成普通 Pod 伪接入。 |

The selected integration has one Kubernetes owner: Kueue owns each native-integration `Workload`. For a Deployment, the source Pod is owned by the Environment-owned ReplicaSet/Deployment; Kueue creates the corresponding Workload and may set only its own Kueue fields. The Workload ownerReference/UID is observed, never synthesized by Resource. Resource persists the association `(ResourceRequestId, CapacityClaimId, LeaseId+revision, Environment generation, source Pod UID, Workload UID)` and uses it as a read/CAS fence; it never creates a second Workload. A retry or recovery first reads the same source object and Workload UID. If either object disappeared, the source owner must complete cleanup and establish a new generation before a new source object is submitted. A late admission for a cancelled or stale UID is rejected by the business generation fence.

### 3. 交互容器与一次性任务

#### 交互 Container Work（Deployment）

Kueue v0.20.2 **不把 Deployment 作为一个原子 Workload**。本契约采用官方 Deployment integration 的 plain-Pod 模式：Deployment 的每一个实际 Pod 都是一个独立的 Kueue PodSet/Workload，使用 Deployment metadata 上的 `kueue.x-k8s.io/queue-name` 选择 LocalQueue；Deployment integration 按副本增减分别准入、释放和抢占。

这意味着：

1. Environment 是 Deployment 和 Pod template 的唯一 writer；Environment 不在 template 中写 Kueue admission 结果。
2. Resource projection 只持久化 approved Resource approval、queue/priority policy、binding 和 generation；Environment 创建 Deployment/Pod/Job 时一次性物化经批准的 immutable labels/priority。Resource 不 patch source object，Environment 不改 approved policy；Kueue webhook/controller 创建并拥有 Workload、scheduling gates、admission assignment 和 eviction/status fields。Environment never bypasses a scheduling gate.
3. scale-out 新 Pod 在 admitted 前保持挂起；scale-in 删除多余 Pod 并释放对应 Kueue 使用量。副本集合不是一个可声称原子成功的商业 Workload；需要原子全副本保证的产品不能使用此模式，必须改用实现了 Kueue integration 的 Job/JobSet/专用 CRD。
4. Scale-out creates a new source Pod identity and therefore a new Kueue Workload identity; scale-in removes only the Environment-owned excess Pod after the business operation is persisted. Resource records each observed Workload UID and never treats a queue/admission status without the matching Resource binding as approval.

一次性执行固定使用 Kueue 的 **`batch/v1 Job` integration**，不使用 plain Pod 或 JobSet。`task-execution`/Evaluation 是 Job 的唯一业务 owner，负责 Job template、immutable execution identity、submit/observe/cancel/cleanup；Kueue native Job integration 负责该 Job 对应 Workload 及 admission/suspension fields。幂等键是 `(TaskRunId, execution_generation, ResourceRequestId, CapacityClaimId, LeaseId+revision)`，数据库 checkpoint 同时保存 Job UID 和 Workload UID。取消先持久化取消事实并通过 Kueue Workload/Job integration 停止 admission，随后观察 Job/Pod 终止并确认 cleanup，最后 Resource release；重复 submit/recovery 复用该键和已有 UID，绝不创建第二个 Job 或第二个 Workload。#212 负责实现该已定接口。


#### 有状态抢占机制：已决定采用 Kueue 原生立即驱逐

用户已决定可抢占的有状态 Work 采用方案 B：Kueue `withinClusterQueue: LowerPriority` 原生立即驱逐。方案 A（禁用 Kueue 自动抢占、由 Resource 先召回）已否决：它无法使用 Kueue 原生公平抢占，并把所有抢占状态机转移到额外的 Resource/Environment 协调器。方案 C（AdmissionCheck/自定义 controller 延迟 eviction）已否决：它需要新的 controller/CRD、权限、恢复和运维面，且会形成第二套抢占握手。

新语义明确如下：

1. Kueue 驱逐只保证 Kubernetes 对 Pod 发送 `SIGTERM`，并等待 Pod 的 `terminationGracePeriodSeconds`；它不调用业务保存 API，也不保证进程内存或尚未提交的文件写入被保存。管理员通过 Environment/Work 配置设定该值；有效范围为 **1–120 秒**，超过 120 秒的配置必须拒绝。默认值为 **30 秒**：PoC 使用 20 秒时观测到 Pod 对象约 22 秒后消失，30 秒为覆盖 API 观察和 kubelet 调度抖动的保守值，同时为 5 分钟窗口保留至少 180 秒用于 fencing、重新准入和连接；30 秒仍只是进程退出宽限，不是数据保存保证。
2. 文件只保证已经落盘/提交的数据；进程内存不保存。应用可选用定期 checkpoint、CoW/快照、写入日志和幂等恢复降低损失，但不得把这些缓解措施声称为驱逐前保存确认。
3. Environment 必须对被动驱逐做对账：Kueue 删除旧 Pod 后，Deployment/Environment 产生的新 UID 在同时满足相同 Resource binding、Environment generation 和 Kueue admission 前必须保持 `SchedulingGated`，不得运行。Environment 记录旧/新 UID 和 Workload UID，拒绝迟到旧 generation 的观察或 admission。
4. Resource 在磁盘 attachment、storage fencing 和旧运行代次不可继续写入尚未确认前，永远不标记 Released、不扩大 quota、不重新借出容量、不结束 physical allocation；也不因 Kueue quota 空闲推断真实释放。清理失败进入 `Unknown/Pending`，由对账恢复。到达 5 分钟失败终态后，退款/补偿可以作为独立 ledger event 入账，但不改变该 allocation 的未确认状态。
5. 5 分钟承诺的唯一时间源是被抢占 source Pod 在 API server 上的 `metadata.deletionTimestamp`；Workload eviction condition 只能作为关联证据。Resource 持久化 `(source_pod_uid, workload_uid, operation_id, eviction_at)` 的 CAS 记录，并设置 `reconnect_deadline = eviction_at + 5m`。到 deadline 前，Environment 必须完成新 generation 的 Resource admission、Kueue admission、实际运行和可连接端点；只重新创建对象或仅获得 Kueue admission 不算可连接。超时即失败，停止继续借出相关容量。
6. 超时/不可用区间退还不可用期间费用，并追加管理员在费率配置中设定的固定补偿。两者都以唯一 `(operation_id, lease_revision, ledger_event_type)` 幂等 ledger event 入账；重复 eviction、重试、旧 UID 观察和迟到 Kueue 状态不得重复退款/补偿。

原生方案 B 对现行条款的影响已由实现范围同步到 `docs/plans/sprint5-refactor-plan.md` 和 Issue #211/#225：60 秒保存承诺删除，替换为 30 秒默认 termination grace + 已落盘文件语义；5 分钟从 eviction 时刻计算；Resource fencing 和账务幂等不变且更严格。

### 4. KubeVirt VM

本 Sprint 不接入 Kueue 的 KubeVirt VMI pod-integration。Kueue v0.20.2 没有 VirtualMachine/VirtualMachineInstance 原生 Workload integration；VMI 的 `virt-launcher` Pod 是 KubeVirt controller 的内部实现细节，直接给该 Pod 加 Kueue labels/gates 或删除它会与 KubeVirt controller 发生双写和重建竞态。

VM 继续由 Resource 做商业准入、预留/lease、费用、召回和 fencing，Environment 经官方 KubeVirt `start`/`stop`/`restart` API 管 VM desired lifecycle、VMI 和持久盘；Kueue 不创建 VM Workload，不管理或删除 VMI/`virt-launcher`。

但“VM 不接入 Kueue”不能意味着两个准入系统共享一份不可见的物理容量。VM 和 Kueue-managed Container 在同一 CPU/GPU/MIG/vGPU 池上线前必须选择并实现下列一种容量边界：

| 选项 | 容量边界 | 代价/状态 |
| --- | --- | --- |
| ① 独立 VM 资源池 | VM 节点使用 `labweaver.io/runtime=vm` taint/node pool；Kueue ResourceFlavor 使用显式 `nodeSelector` 排除该池；Resource VM provider 只选择 VM 池。CPU、独占 GPU、MIG、时间片和 vGPU 均不得跨池借用。 | 已评估但未采用；保留为故障隔离方案。 |
| ② VM 级 Kueue adapter | 新 adapter 以 VM 为原子 PodSet/Workload 参加 Kueue admission，并成为 VM Workload 的唯一 writer。 | 已评估但未采用；需要新增 controller/CRD。 |
| ③ Resource-only VM + 共享 reservation projection | Resource 在同一持久 reservation/fencing 事务中扣除 VM 实际 CPU/GPU/MIG/vGPU 后，只把剩余容量投影到 Kueue ResourceFlavor；每个投影绑定 reservation ID、设备 profile、节点拓扑和 revision。 | **用户选择；#212 实现。**

用户已选择 **③ Resource-only VM + 共享 reservation projection**。Resource 仍是 VM 商业准入唯一权威；VM 不创建 Kueue Workload。统一容量投影的数据流是：Resource 在提交 VM reservation 的同一个持久事务中锁定 `(provider_binding, reservation_id, lease_revision, environment_generation, node/topology, device_profile)`，读取并锁定该 flavor 当前已准入 Kueue Workload 的 usage snapshot/revision（包括正在运行的 PodSet 资源），计算扣除 VM 已批准 CPU/GPU/MIG/vGPU 后的剩余量；projection controller 仅以同一 reservation ID/revision CAS 更新 ClusterQueue quota 快照，并记录投影版本。Resource reservation commit 只有在“已准入 usage + 新 VM reservation <= 受管物理容量”不变量成立时才允许提交；容量不足时拒绝 VM reservation，或先走受支持的 Kueue eviction/requeue、Environment fencing 和确认释放，再重新尝试。最终一致的 quota 对账不能替代该提交前锁定。

投影必须满足：资源名、数量、设备 profile、拓扑约束、可借用期限和 reservation revision 一一对应；提交前读取/锁定的 admitted usage snapshot 发生 revision 变化、观察过期、VM fencing 未确认、投影更新失败或 Kueue status 与快照不一致时，projection 标记 stale 并 fail closed，不准入新 Work、不扩大 quota、不重新借出 allocation，直到 Resource 权威对账确认。对账按 reservation ID + revision + Kueue object UID CAS 更新；迟到的 Kueue admission/eviction 不得复活旧 generation。#212 实现该 projection interface、admitted-usage lock/CAS、eviction/requeue fallback 和 recovery；#213/#214 必须先提供 GPU profile/device-plugin/KubeVirt mdev 目录，全部模式通过真实设备验证后才能上线。

KubeVirt 真实集群未在本次 Kind PoC 安装，故 VM 结论仍是设计边界而非运行通过。

### 5. 可回收借用、原生驱逐与 5 分钟重启

Kueue 的 quota 是 Kubernetes admission accounting，不是 Resource 的售出库存。Resource 保持三种独立事实：

1. **售出预留**：预留期间持续占用商业库存；停止或暂借都不能再次售卖。
2. **当前物理分配**：节点/设备、运行代次、盘 attachment 和观察/fencing 状态。
3. **可借用容量**：只有预留仍有效、当前物理容量可安全释放、且 Work 具备被动 eviction 对账和 fencing 能力时，才可投影成 Kueue 可抢占配额。

可回收借用的时序固定为：

1. Resource 记录召回操作，持久化 `operation_id`、ResourceRequest/CapacityClaim/Lease revision、Work generation、借用 Workload UID、source Pod UID、管理员配置的 `terminationGracePeriodSeconds`（默认 30 秒，最大 120 秒）、以及阶段。Resource 只接受 source Pod API 的 `metadata.deletionTimestamp` 作为 `eviction_at`，以 `(source Pod UID, Workload UID, operation_id)` 做持久 CAS；Workload condition 只作关联证据。成功记录后设置 `reconnect_deadline = eviction_at + 5m`，重复 eviction 事件复用同一 operation ID。
2. Kueue 原生 eviction 删除旧 Pod 并发送 SIGTERM；Environment 读取 deletionTimestamp、旧/新 Pod UID 和 Workload UID。新 Pod 即使由 Deployment 重建，也必须在匹配 Resource binding、Environment generation 和 Kueue admission 前保持 SchedulingGated；只创建对象不算运行。
3. Environment 保存已落盘/提交的文件并观察 VM/容器状态；进程内存不保存。Resource 观察 attachment、storage fencing 和旧运行代次不可继续写入。响应成功、Pod 消失、termination grace 到期或 Kueue status 更新本身都不是释放确认。
4. 只有 fencing 和清理确认后，Resource 才将借用标记 Released，更新账务并允许新的 generation 重新准入；Resource 不直接 patch Kueue Workload，不因 quota 空闲确认物理释放。fencing 未知时保持 reservation 和容量不可借出。
5. 新高优先级 Work 经过 Resource reservation projection 与 Kueue admission 后才可运行；可借用 Work 释放的是当前物理使用，不是预留库存或预付费用。
6. 从 `eviction_at` 到新 generation 完成 Resource admission、Kueue admission、实际运行和可连接端点最多 5 分钟。超时、停止失败、权限拒绝、节点失联或 attachment/fencing 未知时，停止继续借出相关容量；allocation 仍保持 `Unknown/Pending`，不得 Released、不得扩大 quota、不得重新借出。达到失败终态后，Resource 可独立追加“eviction 后不可用期间退款 + 管理员固定补偿” ledger event；该事件不改变 allocation 的未确认状态，每项以唯一 `(operation_id, lease_revision, ledger_event_type)` 幂等入账。

Kueue `preemption` policy 只决定 Kubernetes 层队列公平和候选选择，不能表达或推导 Resource 账务。项目权重/老化、Resource 的预留优先级、Credits 和补偿由 Resource policy 决定并投影为源 Pod/Job 的 Kueue-supported queue/priority；Kueue 的 `cohort`/`borrowing` 只代表 Kubernetes 层暂借 quota，不能扩大 Resource 可售库存。

### 资源映射、公平性和 GPU 依赖

Resource 是资源名和数量的权威来源；Kueue 只消费 projection 的快照。#212 必须使用以下一一映射，不允许按“看起来可用”的资源名静默替换：

| Resource catalog 事实 | Kueue 投影 | 约束 |
| --- | --- | --- |
| CPU、内存 | `coveredResources: cpu,memory`，请求使用 Kubernetes 原生 `cpu`/`memory` | 数量和单位从 approved claim 转换；转换失败或超过快照即不准入。 |
| Pod 数 | `coveredResources: pods` | `pods` 是 Kueue 保留资源名，由 Kueue 从 PodSet 计算，不能出现在 Pod requests。 |
| 容器独占 GPU | Resource catalog 绑定设备插件实际扩展资源名和 profile；对应 Kueue ResourceFlavor | 只有同一 profile、节点标签/taint 和 topology 的 flavor 可用；不把普通 `nvidia.com/gpu` 静默当成另一 profile。 |
| 容器 MIG/时间片 GPU | 分别绑定设备插件实际 MIG profile 或时间片资源名；每个模式独立 flavor/配额 | 模式和扩展资源名不能互换；份额不代表独占显存或算力。 |
| VM vGPU/mdev | 不创建 Kueue VM Workload；由统一 reservation projection 扣除对应 VM profile 后更新容器 quota 快照 | profile、驱动、许可、节点拓扑或 mdev 观察不一致时 fail closed；不得回退到普通 GPU、Mock 或另一模式。 |

ResourceFlavor 的 `nodeLabels`/拓扑约束只表达 Resource 已批准的 placement domain；kube-scheduler 仍是最终放置者。Project 权重、老化和预留优先级先由 Resource 计算并投影为 Kueue-supported queue/priority；Kueue 的公平队列只在该快照内排序，不能越过 Credits、商业资格或 reservation fencing。quota 使用分页/watch/relist 对账，观察过期、投影 revision 落后或 Kueue admission 与快照不一致时停止新 admission。

GPU 独占、MIG、容器时间片和 VM vGPU 的目录/profile、设备插件资源名及 KubeVirt mdev 约束依赖 #213/#214。#212/#213/#214 的全部模式完成真实设备验证、容量 projection CAS 和失败注入前，GPU/VM 共享容量禁止上线。

## 字段与事件唯一 writer

| 事件/字段 | Resource projection | Environment/KubeVirt | Kueue |
| --- | --- | --- | --- |
| ResourceRequest/Approval/Claim/Lease/revision | 写入、CAS fencing、账务 | 只读消费 | 不可见为商业事实 |
| queue/priority/preemptible/ResourceFlavor 映射 | Resource 只持久化 approved policy、reservation projection、binding/revision；不 patch source object | Environment 创建 source Pod/Job 时一次性物化 immutable labels/priority，不改 approved policy | Kueue native integration creates/owns Workload `spec`/`status`, admission, assignment and eviction |
| Deployment template/replicas/desired generation | 不写 | Environment 唯一写 | 只写 gate/观察关联，不写业务 desired state |
| Pod scheduling gate/admission annotation | 不写业务 template | 不删除 Kueue gate | Kueue webhook/controller 写 |
| VM start/stop/restart、VMI、virt-launcher、PVC attachment | Resource 授权并观察 | Environment/KubeVirt 唯一写 | 不写、不建 Workload |
| Eviction 时间、30 秒默认 termination grace、5 分钟 deadline、运行代次和盘 fencing | Resource 持久化 policy/fact；Environment 执行并回报 | Kueue 发送 SIGTERM、设置 eviction/status；不确认业务保存、fencing、退款或补偿 |
| 借用 release、费用、退款、补偿 | Resource 唯一写 | 回报实际释放证据 | Workload admission/eviction 仅在 projection 允许后更新 |

- `Workload` 已 admitted 但 Environment/task-execution 创建或观察失败：保持业务状态为 pending/failed，按已有 source UID 做确定性清理；不把 admission 当成功，不创建第二个 Job/Pod/Workload。恢复必须先匹配持久的 Workload UID 和 binding revision。

- 任何丢失/过期/不一致的 Resource binding、Lease revision、Work generation、queue binding 或 namespace 都 fail closed；不因 Kueue admission 存在而创建运行对象。
- Eviction、重建 UID、停止、fencing、release 和 ledger event 都必须按 operation ID + generation 幂等；重启恢复持久 `eviction_at`/`reconnect_deadline`，不得重置 5 分钟窗口。
- Kueue eviction、Deployment 重建新 UID、VMI/virt-launcher 重建、节点失联或 attachment 状态未知，均进入 `Unknown/Pending`，不得标记 Released 或新准入；5 分钟超时只表示重启承诺失败，不表示 fencing 成功。
- Resource/Kueue projection 快照不一致时，以 Resource 的 reservation/revision 和 Environment 的权威运行/fencing 事实为准；projection 标 stale、停止新 admission、保留现有 reservation，不 patch Workload、不创建第二套 lease，按 reservation ID + revision + Workload UID CAS 对账后才恢复。

## 验证边界和 PoC

### 固定来源

- Kueue v0.20.2 README：官方支持 Kubernetes 1.34+，并列出 1.35 baseline/extended Kind 验证；Workload API 为 v1beta2。
- [Kueue plain Pod integration](https://kueue.sigs.k8s.io/docs/tasks/run/plain_pods/)：Pod 被注入 scheduling gate，准入后移除；抢占会终止并删除 Pod。
- [Kueue Deployment integration](https://kueue.sigs.k8s.io/docs/tasks/run/deployment/)：Deployment 不是单一 Workload；每个 Pod 是独立 plain-Pod Workload，scale-out 的新 Pod 在准入前挂起。
- [Kueue v0.20.2 install manifest](https://github.com/kubernetes-sigs/kueue/releases/download/v0.20.2/manifests.yaml)：CRD 与 controller 安装源。
- `deploy/versions.lock.yml`：本分支记录 Kubernetes v1.35.6；v1 目标为 v1.35.8，均位于 Kueue v0.20.2 支持的 Kubernetes 1.35 线。

### 实际 Kind PoC（2026-10-23）

集群名固定为 `lw-kueue-poc`，脚本结束时通过 `trap` 删除集群，未使用或修改 `tools/local_dev.py`。可复现入口和完整 fixture 已提交：

- `tools/kueue_poc/run.sh`
- `tools/kueue_poc/fixture.yaml`
- Kueue manifest URL：`https://github.com/kubernetes-sigs/kueue/releases/download/v0.20.2/manifests.yaml`
- manifest SHA-256：`1ab14ecd590575ac57f50fa4cbcf529766bc2c32ba974bda1614381281ec52b4`
- Kueue image：`registry.k8s.io/kueue/kueue:v0.20.2@sha256:36cf91ae547395836c75c279d38266ed0c5b2b42ea7c82aaf3f990c7a89dfc1b`
- signal-test image：`busybox:1.36.1@sha256:73aaf090f3d85aa34ee199857f03fa3a95c8ede2ffd4cc2cdb5b94e566b11662`

实际执行：

```text
$ tools/kueue_poc/run.sh
== create lw-kueue-poc ==
✓ Ready after 31s 💚
== fetch and verify Kueue manifest ==
/tmp/.../kueue-manifests.yaml: OK
== install Kueue ==
deployment "kueue-controller-manager" successfully rolled out
== apply quota and interactive fixture ==
localqueue.kueue.x-k8s.io/poc-queue created
deployment.apps/low-interactive condition met
low admitted name=low-interactive-5d855b47fb-s8bmn uid=1ff64b70-92c5-4d41-aed1-40558ec7b522
== evaluate native Kueue preemption and SIGTERM grace ==
deployment.apps/high-interactive scaled
Kueue eviction set deletionTimestamp; SIGTERM log=<not observed before object deletion>
Pod object removal observed after approximately 22s; terminationGracePeriodSeconds=20s
deployment.apps/high-interactive condition met
high phase=Running; replacement low state=low-interactive-5d855b47fb-h5445 ... Pending kueue.x-k8s.io/admission
PASS: native preemption is immediate; SIGTERM had only terminationGracePeriodSeconds=20s, with no business save guarantee
Normal Preempted ... Preempted to accommodate a workload ... preemptor effective priority: 100 ... preemptee effective priority: 10
Normal Stopped ... Preempted to accommodate a workload ...
POC PASS
```

Kind 使用 `kindest/node:v1.35.0`（kubectl client v1.35.8，server v1.35.0）。Fixture 配置 `cpu=1`、`memory=1Gi`、`pods=1`，低优先级 `10` 和高优先级 `100` 各请求 `900m/128Mi`，`withinClusterQueue: LowerPriority`，低 Pod 设置 `terminationGracePeriodSeconds: 20` 并安装 SIGTERM trap。结果证明：低 Pod 先被准入；高 Work 触发原生 `PreemptedWorkload`，低 Pod 获得删除时间戳并约 22 秒后消失；Deployment 创建的新低 Pod 保持 `SchedulingGated`，高 Pod Running。SIGTERM 期间未能从 API 读到 trap 日志，但对象删除窗口与 20 秒 termination grace 相符；无证据表明业务保存完成。该结果是原生立即驱逐方案的实测证据，文件/内存语义和被动 fencing 对账仍由 #212 实现。

KubeVirt 未安装：一节点 Kind 无虚拟化设备，且 Kueue v0.20.2 没有内建 VMI/VirtualMachine Workload integration。本次 VM 结论是 Resource reservation projection 的设计契约，不是 KubeVirt 运行验收。
### Acceptance conclusions for #211

| 验收项 | 结论 | 证据/剩余风险 |
| --- | --- | --- |
| 对象/字段唯一管理表和事件时序，覆盖容器、VM、一次性任务 | **完成（设计，实施归 #212）** | Kueue native integration 唯一拥有 Workload object/spec/status/UID；Environment/task-execution 拥有源 Pod/Job；Resource 保存 binding+source UID+Workload UID 关联和 CAS fence；VM 使用 Resource reservation projection。 |
| 受支持接口证明暂停/准入/抢占不会被 Deployment/VMI 立即重建 | **容器原生 eviction 已完成；VM 未验证** | 可复现 PoC 证明 Kueue `LowerPriority` 立即 eviction，约 22 秒后 Pod 删除（20 秒 terminationGrace）；Deployment 创建的新 UID 保持 `SchedulingGated`，高优先级 Pod Running。ADR 已将语义定为 SIGTERM + 默认 30 秒 terminationGrace，不保证业务保存或进程内存；VMI 未安装/未验证。 |
| 容器和 VM 使用同一商业准入规则；不篡改 virt-launcher | **设计完成；运行未验收** | Resource 是 VM/容器商业权威；VM 通过同一 reservation/fencing 事务投影 Kueue quota；GPU/MIG/vGPU 依赖 #213/#214，真实设备验证前禁止共享容量上线；无 virt-launcher 修改。 |
| 依赖、限定版本和失败处理明确后进入 #212 | **设计完成；实现仍归 #212** | Kueue v0.20.2/v1beta2、Workload owner、统一 capacity projection、eviction-to-connect 5m、默认 30s termination grace、被动 UID/fencing 对账、退款与固定补偿 ledger 语义已明确；#212 仍需实现接口、CAS、对账、节点失联/fencing 和 native eviction integration。

## 替换和回滚

本 ADR 仅新增文档，不新增 CRD、Migration、API 或运行代码。Kueue 版本、VM integration 或 Workload owner 变更必须由 superseding ADR 说明 CRD/storage 迁移、对象所有权、回滚和已有 lease/账务的影响；不得保留一个静默的旧调度器作为 fallback。
