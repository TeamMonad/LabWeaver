# ADR 0018：Kueue 准入、交互 Work 与可回收借用边界

状态：Issue #211 的实施契约；本 ADR 锁定 Kueue API 与对象所有权，准入实现和业务账务实现分别属于 #212 及后续 Issue。人类架构/安全 Review 由 A+B 完成；本 ADR 的技术证据见末尾 PoC 记录。

日期：2026-10-22

## 背景

Sprint5 同时提供有保证容量的预留和可回收借用的可抢占 Work。Resource 已经拥有商业资格、审批、CapacityClaim、Lease、配额、用量和费用；Environment 已经拥有长生命周期 Work 的运行代次、实例、持久盘和端点。Kueue 只应承担 Kubernetes 层的队列配额、公平性、准入和 Kueue 语义的驱逐，不能成为第二个 Resource，也不能成为 Environment 的第二个运行对象控制器。

Kubernetes 同一资源只能有一个管理者。Kueue 的 Pod integration 会为 Pod 注入 scheduling gate，并在 Workload admitted 后移除 gate；在抢占时会终止并删除被抢占的 Pod。因此，Kueue 不得直接删除 Environment 的 Deployment、VM、磁盘或 `virt-launcher`，也不得在 Resource 的 60 秒保存宽限完成前把一次驱逐解释为商业容量已释放。

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
| 召回通知、保存截止、召回操作的运行代次和 Resource fencing | **Resource**（持久状态；调用 Environment 执行） | Kueue 只能接收 Resource 已授权的 Workload 投影；不能自己启动 60 秒计时或退款。 |
| Work/Environment 的 desired revision、运行 plan、Deployment、VM、PVC/磁盘挂载、端点和运行代次 | **Environment**（Environment executor 代表 Environment 写 Kubernetes 对象） | Kueue 不能写 Deployment/VM/PVC 的业务字段，也不能创建 Environment 的第二份实例。 |
| Kueue `Workload` 的 queue、PodSet、priority、admission、active/eviction 状态和 Kueue queue 绑定字段 | **Resource 的 Kueue projection/controller**（唯一投影 writer） | 该控制器只把 Resource 的已批准 policy 映射为 Kueue 对象；业务服务不直接理解 ClusterQueue/LocalQueue/ResourceFlavor。 |
| Pod 到节点的绑定和实际放置 | **kube-scheduler** | LabWeaver 不增加 Pod-to-Node scheduler。ResourceFlavor 只表达受 Resource 认可的拓扑/设备类别。 |
| VMI、`virt-launcher` Pod 及其 guest 生命周期 | **KubeVirt controllers**，由 Environment 通过官方 VM 生命周期 API 请求 | LabWeaver 不 patch 或 delete `virt-launcher`，不把 VMI 当成普通 Pod 伪接入。 |

Resource projection 以一个稳定的 ResourceRequest/CapacityClaim/Lease + revision、project、Work/TaskRun、provider、namespace 和运行代次建立一个 Kueue Workload。重试或恢复复用同一个未释放的商业绑定；新执行代次使用新的 Resource 绑定。Environment 只消费已经同时满足 Resource binding 和 Kueue admission 的授权，不因 Pod 存在或 Kueue status 推断商业批准。

### 3. 交互容器与一次性任务

#### 交互 Container Work（Deployment）

Kueue v0.20.2 **不把 Deployment 作为一个原子 Workload**。本契约采用官方 Deployment integration 的 plain-Pod 模式：Deployment 的每一个实际 Pod 都是一个独立的 Kueue PodSet/Workload，使用 Deployment metadata 上的 `kueue.x-k8s.io/queue-name` 选择 LocalQueue；Deployment integration 按副本增减分别准入、释放和抢占。

这意味着：

1. Environment 是 Deployment 和 Pod template 的唯一 writer；Environment 不在 template 中写 Kueue admission 结果。
2. Resource projection 为每个 Deployment Pod 代次建立 workload identity，并将相同 Resource policy、priority、preemptible 和 queue binding 投影到对应的 Kueue Workload。Kueue 注入/移除 scheduling gate；Environment 不绕过 gate 启动未准入 Pod。
3. scale-out 新 Pod 在 admitted 前保持挂起；scale-in 删除多余 Pod 并释放对应 Kueue 使用量。副本集合不是一个可声称原子成功的商业 Workload；需要原子全副本保证的产品不能使用此模式，必须改用实现了 Kueue integration 的 Job/JobSet/专用 CRD。
4. 交互容器的保存和召回仍是 Environment/Resource 协作：先通知并持久化保存点，随后 Environment 以 UID/resourceVersion 条件停止；仅在观察到 Pod 已停止、磁盘已隔离且运行代次仍匹配后，Resource 才能把借用释放事实写账并允许新的 admission。

一次性 Job/Execution 使用其正式 Kueue integration（Job 或 plain Pod integration），仍遵守 ADR 0016 的 `submit → observe → cancel → cleanup` 和“结果持久化、清理确认、最后释放 Resource”顺序。Kueue admission 不替代 `TaskExecutionBinding`。

#### 自动重建禁止条件

Kueue 的抢占会删除 Pod；Deployment 控制器会按 desired replicas 创建替代 Pod。因此，Kueue 对 Deployment Pod 的直接抢占**不能**作为有状态 Work 的 60 秒召回实现：如果 Environment 没有先将 desired state 置为停止/召回并持久化 fencing，Deployment 会重建未授权的新 UID。#212 必须在 Resource/Environment projection 中先关闭重建路径，再让 Kueue 的 Workload 进入 eviction/finish；观察必须确认旧 Pod 消失且没有新 Pod 运行，不能只看一个删除响应。

### 4. KubeVirt VM

本 Sprint 采用 **不接入 Kueue 的 KubeVirt VMI pod-integration**。Kueue v0.20.2 的内建 integrations 和官方 serving/batch 文档不提供 VirtualMachine/VirtualMachineInstance 的原生 Workload integration；VMI 的 `virt-launcher` Pod 是 KubeVirt controller 的内部实现细节，直接给该 Pod 加 Kueue labels/gates 或删除它会与 KubeVirt controller 发生双写和重建竞态。

因此：

- VM 继续走 **Resource 现有准入**：Resource 管商业资格、预留/lease、费用、召回和 fencing；Environment 经官方 KubeVirt `start`/`stop`/`restart` API 管 VM desired lifecycle、VMI 和持久盘。
- Kueue 不为 VM 创建 Workload，不管理 VMI/virt-launcher，不删除 virt-launcher，不把 `VirtualMachineInstance` 映射为一个看似可准入的 plain Pod。
- VM 与容器仍使用相同的 Resource 商业规则、priority/preemptible policy 和召回时序，但 VM 的 capacity/admission backend 是 Resource 的 VM provider，而不是 Kueue。Resource 只有在 VM 停止、盘隔离、fencing 和持久观察确认后才释放可借用物理容量。
- 若未来需要 Kueue 管理 VM，必须另行引入并验证官方 KubeVirt integration 或一个维护 VM 级原子语义的 adapter CRD；该 adapter 将成为 VMI/Workload 的唯一 writer，并需要 superseding ADR，不得在 #212 中暗中添加 pod-integration 兼容层。

本 ADR 对 KubeVirt 的结论是设计边界，不是 KubeVirt 真实集群验收；本次 Kind PoC 未安装 KubeVirt，原因和未验证项见 PoC 记录。

### 5. 可回收借用、抢占与 60 秒宽限

Kueue 的 quota 是 Kubernetes admission accounting，不是 Resource 的售出库存。Resource 保持三种独立事实：

1. **售出预留**：预留期间持续占用商业库存；停止或暂借都不能再次售卖。
2. **当前物理分配**：节点/设备、运行代次、盘 attachment 和观察/fencing 状态。
3. **可借用容量**：只有预留仍有效、当前物理容量可安全释放、且 Work 具备召回能力时，才可投影成 Kueue 可抢占配额。

可回收借用的时序固定为：

1. Resource 记录召回操作（ResourceRequest/CapacityClaim/Lease revision、Work generation、借用 Workload UID），发送一次幂等通知，并持久化 `deadline = notified_at + 60s`；服务重启从该记录恢复，不能以内存 timer 代替。
2. 60 秒内，Environment 保存容器工作文件/VM 盘状态，撤销连接并将 desired state 置为停止；不删除持久盘，不把 Kueue 的 eviction 当作保存完成。
3. 到期后，Environment 通过带 UID/resourceVersion 的官方 API 停止 Deployment/VM，并隔离盘；Resource 观察权威对象、Pod/VMI、attachment 和必要的 storage fencing。响应成功、Pod 消失或 Kueue status 更新本身都不是释放确认。
4. 只有停止和隔离被确认、旧运行代次不可再写入后，Resource 才将借用标记 Released，更新费用/补偿账目，并允许 Kueue 释放旧 Workload 的 admission。Kueue 的 `active=false`、eviction 或 Workload 删除由 projection controller 在该业务事实之后执行；不能由 Kueue 直接执行商业 release。
5. 新的高优先级 Work 先经过 Resource 商业资格和容量 fencing，再建立新的 Kueue Workload/admission。Resource 的预留权仍保留；可借用 Work 释放的是当前物理使用，不是预留库存或预付费用。
6. 召回失败、停止超时、权限拒绝、节点失联、storage attachment/fencing 未知或超过 5 分钟重启承诺时：禁止继续借出该容量；保持售出预留但不把容量标为可用；对不可用期间按产品规则退款并记录管理员配置的补偿。禁止 Kueue 自动删除 Pod 后立即重新准入，也禁止因 Kueue quota 已空闲而退款或确认物理释放。

Kueue `preemption` policy 只用于队列公平性和候选选择。项目权重/老化、Resource 的预留优先级、Credits、补偿和 60 秒延迟由 Resource policy 决定并投影为 Kueue priority/queue 配置；Kueue 不能表达或推导账务。Kueue 的 `cohort`/borrowing 只代表 Kubernetes 层暂借 quota，不能扩大 Resource 可售库存。

## 字段与事件唯一 writer

| 事件/字段 | Resource projection | Environment/KubeVirt | Kueue |
| --- | --- | --- | --- |
| ResourceRequest/Approval/Claim/Lease/revision | 写入、CAS fencing、账务 | 只读消费 | 不可见为商业事实 |
| queue/priority/preemptible/PodSet/ResourceFlavor 映射 | 写入 Kueue projection 的 desired 对象 | 不写 | controller 写 admission、assignment、eviction 状态 |
| Deployment template/replicas/desired generation | 不写 | Environment 唯一写 | 只写 gate/观察关联，不写业务 desired state |
| Pod scheduling gate/admission annotation | 不写业务 template | 不删除 Kueue gate | Kueue webhook/controller 写 |
| VM start/stop/restart、VMI、virt-launcher、PVC attachment | Resource 授权并观察 | Environment/KubeVirt 唯一写 | 不写、不建 Workload |
| 保存通知、60 秒 deadline、停止和盘 fencing | Resource 持久化 policy/fact；Environment 执行并回报 | Environment 写运行事实 | 不计时、不确认释放 |
| 借用 release、费用、退款、补偿 | Resource 唯一写 | 回报实际释放证据 | Workload admission/eviction 仅在 projection 允许后更新 |

## 失败处理和不变量

- 任何丢失/过期/不一致的 Resource binding、Lease revision、Work generation、queue binding 或 namespace 都 fail closed；不因 Kueue admission 存在而创建运行对象。
- `Workload` 已 admitted 但 Environment 创建/观察失败：保持业务状态为 pending/failed，执行确定性清理；不把 admission 当成功，不重复创建第二个 Workload。
- 召回通知、保存、停止、eviction 和 release 都必须按操作 ID + generation 幂等；重启恢复持久 deadline，不重置 60 秒。
- Kueue eviction 早于 Resource deadline、Deployment 重建新 UID、VMI/virt-launcher 重建、节点失联或 attachment 状态未知，均进入 `Unknown/Pending`，不得标记 Released 或新准入。
- Resource 和 Kueue 状态不一致时，以 Resource 的商业事实和 Environment 的权威运行/fencing 事实为准；projection controller 重试对账，不创建第二套 lease 或 Workload。

## 验证边界和 PoC

### 固定来源

- Kueue v0.20.2 README：官方支持 Kubernetes 1.34+，并列出 1.35 baseline/extended Kind 验证；Workload API 为 v1beta2。
- [Kueue plain Pod integration](https://kueue.sigs.k8s.io/docs/tasks/run/plain_pods/)：Pod 被注入 scheduling gate，准入后移除；抢占会终止并删除 Pod。
- [Kueue Deployment integration](https://kueue.sigs.k8s.io/docs/tasks/run/deployment/)：Deployment 不是单一 Workload；每个 Pod 是独立 plain-Pod Workload，scale-out 的新 Pod 在准入前挂起。
- [Kueue v0.20.2 install manifest](https://github.com/kubernetes-sigs/kueue/releases/download/v0.20.2/manifests.yaml)：CRD 与 controller 安装源。
- `deploy/versions.lock.yml`：本分支记录 Kubernetes v1.35.6；v1 目标为 v1.35.8，均位于 Kueue v0.20.2 支持的 Kubernetes 1.35 线。

### 实际 Kind PoC（2026-10-22）

集群名固定为 `lw-kueue-poc`，未使用或修改 `tools/local_dev.py` 的集群。KubeVirt 未安装；Kind 节点不具备 KubeVirt 所需的虚拟化设备，且本次目标是验证 Kueue 的容器边界。完整命令与关键输出如下。

```text
$ kind create cluster --name lw-kueue-poc --image kindest/node:v1.35.0 --wait 120s
✓ Starting control-plane ...
✓ Ready after 19s
Set kubectl context to "kind-lw-kueue-poc"

$ curl -L --retry 5 --retry-delay 3 --connect-timeout 20 --max-time 300 \\
    -o /tmp/kueue-v0.20.2-manifests.yaml \\
    https://github.com/kubernetes-sigs/kueue/releases/download/v0.20.2/manifests.yaml
$ kubectl --context kind-lw-kueue-poc apply --server-side \\
    -f /tmp/kueue-v0.20.2-manifests.yaml
namespace/kueue-system serverside-applied
customresourcedefinition.apiextensions.k8s.io/workloads.kueue.x-k8s.io serverside-applied
...
deployment.apps/kueue-controller-manager serverside-applied

# The registry.k8s.io pull timed out in the Kind node. The same pinned image
# was pulled on the host and imported into containerd; imagePullPolicy=Never
# was used only for this disposable PoC.
$ docker pull --platform=linux/amd64 registry.k8s.io/kueue/kueue:v0.20.2
Digest: sha256:36cf91ae547395836c75c279d38266ed0c5b2b42ea7c82aaf3f990c7a89dfc1b
$ docker save kueue-poc:v0.20.2 | docker exec -i lw-kueue-poc-control-plane \\
    ctr --namespace=k8s.io images import --digests --snapshotter=overlayfs -
$ kubectl --context kind-lw-kueue-poc -n kueue-system \\
    rollout status deployment/kueue-controller-manager --timeout=180s
deployment "kueue-controller-manager" successfully rolled out

$ kubectl --context kind-lw-kueue-poc version --short
Client Version: v1.35.8
Kustomize Version: v5.7.1
Server Version: v1.35.0

$ kubectl --context kind-lw-kueue-poc get crd workloads.kueue.x-k8s.io \\
    -o jsonpath='{.spec.versions[*].name}{" storage="}{range .spec.versions[*]}{.storage}{" "}{end}{"\\n"}'
v1beta1 v1beta2 storage=false true
```

The quota fixture used one `ResourceFlavor`, one `ClusterQueue` with `cpu=1`, `memory=1Gi`, `pods=1`, one `LocalQueue`, and `withinClusterQueue: LowerPriority`. Both Deployments requested `900m` CPU and `128Mi` memory and selected the same queue; the high-priority Deployment used `poc-high=100` and the low-priority Deployment used `poc-low=10`.

Admission, suspension, and preemption were observed with these real commands:

```text
$ kubectl --context kind-lw-kueue-poc -n kueue-poc wait \\
    --for=condition=available deployment/low-interactive --timeout=120s
deployment.apps/low-interactive condition met

$ kubectl --context kind-lw-kueue-poc -n kueue-poc scale deployment high-interactive --replicas=1
$ kubectl --context kind-lw-kueue-poc -n kueue-poc wait \\
    --for=condition=available deployment/high-interactive --timeout=180s
deployment.apps/high-interactive condition met

$ kubectl --context kind-lw-kueue-poc -n kueue-poc get deployments,pods,workloads.kueue.x-k8s.io -o wide
deployment.apps/high-interactive   1/1   1   1
deployment.apps/low-interactive    0/1   1   0
pod/high-interactive-...           1/1   Running
pod/low-interactive-...            0/1   SchedulingGated
workload.kueue.x-k8s.io/pod-high-interactive-...   poc-queue   poc-cq   True
workload.kueue.x-k8s.io/pod-low-interactive-...    poc-queue             False

$ kubectl --context kind-lw-kueue-poc -n kueue-poc get events --sort-by=.lastTimestamp
Normal  PreemptedWorkload  ...  Preempted workload ... in ClusterQueue poc-cq;
                                  preemptor effective priority: 100 ... preemptee ... 10
Warning WaitingForPreemptedWorkloads ... Pending the preemption of 1 workload(s)
```

The first low-priority Deployment Pod was admitted and reached `Running`. On scale-out of the high-priority Deployment, Kueue evicted/deleted the low-priority Pod; the Deployment controller immediately created a replacement Pod, which remained `SchedulingGated`, while the high-priority Pod was `Running` and its Workload was `Admitted=True`. This proves the required Kueue mechanics and also proves why Environment must first change desired state before using this path for a stateful Work: a plain Deployment controller otherwise recreates an unauthorized Pod.

No KubeVirt installation or VMI test was attempted. Installing KubeVirt/virt-operator/virt-controller plus CDI and a functional virtualization device on a one-node Kind cluster would not prove the supported VM integration (Kueue v0.20.2 has no built-in VMI/VirtualMachine Workload integration), and would add an unrelated virtualization setup to this bounded Kueue PoC. The VM conclusion is therefore an API/ownership boundary, not a KubeVirt runtime pass.

### Acceptance conclusions for #211

| 验收项 | 结论 | 证据/剩余风险 |
| --- | --- | --- |
| 对象/字段唯一管理表和事件时序，覆盖容器、VM、一次性任务 | **完成（设计）** | 本 ADR 的 owner 表、Deployment plain-Pod 映射、VM 不接入、Job/Execution 及召回时序。 |
| 受支持接口证明暂停/准入/抢占不会被 Deployment/VMI 立即重建 | **容器部分完成；VM 未验证** | Kind v1.35.0 控制面运行 Kueue v0.20.2；低优先级 Deployment Pod 先 Running，高优先级扩容触发 `PreemptedWorkload`，旧 Pod 被删除，Deployment 立即重建的新 Pod 保持 `SchedulingGated`，高优先级 Pod Running 且 Workload Admitted=True。该结果证明必须先关闭 Environment desired state 才能安全召回。VMI 未安装/未验证。 |
| 容器和 VM 使用同一商业准入规则；不篡改 virt-launcher | **完成（设计），VM 运行未验收** | Resource 统一商业 policy；Kueue 仅容器 Kubernetes admission；VM 继续 Resource + Environment/KubeVirt 官方 API；无 virt-launcher 修改。 |
| 依赖、限定版本和失败处理明确后进入 #212 | **完成（设计与容器 PoC）；正式集成仍是 #212 范围** | Kueue v0.20.2/v1beta2、Kubernetes 1.35 兼容性、唯一 writer、60 秒/5 分钟失败补偿均明确。#212 可据此实现，但必须补 Resource/Environment integration、重启/节点失联/fencing 和 KubeVirt 真实证据，不把本 PoC 当作商业准入验收。

## 替换和回滚

本 ADR 仅新增文档，不新增 CRD、Migration、API 或运行代码。Kueue 版本、VM integration 或 Workload owner 变更必须由 superseding ADR 说明 CRD/storage 迁移、对象所有权、回滚和已有 lease/账务的影响；不得保留一个静默的旧调度器作为 fallback。
