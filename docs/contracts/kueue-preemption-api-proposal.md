# Kueue 抢占事件与现有执行 API 对照（proposal）

本文件是 #211 后续实现的接口核查，不是已接通的控制循环。基线为 develop `cc39763ef3657833223a4e8dbfed9d6f2da7ac08`；已核 PR258 `550f5bb7ff69f2b6a1ccb5db936a6e2b2c70be47` 的 `services/environment-service/src/runtime_executor.rs`，未把该 PR 内容复制或合并进本分支。已封存模型提交为 `a440848536ec1b8a4a15b25cc19280bd3a97b65c`。

`deploy/versions.lock.yml` 已锁 Kubernetes v1.35.6、KubeVirt v1.8.4、CDI v1.65.0。Kueue 尚未锁定，本文不新增版本配置。Resource 管商业资格、召回和账务；Kueue 管队列配额/公平；Environment 唯一管理运行代次和磁盘挂载；kube-scheduler 管节点放置。

## 模型事件到现有 API

| 模型事件 | 容器入口和字段 | VM 入口和观察 | 一次性任务入口 | 仍缺的证明 |
| --- | --- | --- | --- | --- |
| notify | 没有在 executor 中找到用户通知/保存计时入口；不能把 Scale 当通知 | 没有在 VM 子资源中找到商业召回通知入口 | 业务任务 Owner 协调保存/取消 | Resource 召回身份、配置 revision、通知与60秒计时的持久化映射 |
| stop | `ContainerExecutorRequest::Scale { replicas: 0 }` → `scale` → `stop_container`，带 UID/resourceVersion 条件 PATCH Deployment `spec.replicas=0` | `KubeVirtExecutorRequest::Stop` → `kubevirt_lifecycle_subresource(...,"stop",...)`，PUT `/apis/subresources.kubevirt.io/v1/namespaces/{namespace}/virtualmachines/{name}/stop` | `KubernetesApiClient::cleanup` 的归属检查及 UID 前置删除不是业务保存通知；不能在60秒前用清理代替召回 | 先等保存宽限，再进入获授权的停止；本文件没有发请求 |
| observe_release | `stop_container` 等同 UID、replicas=0、observedGeneration追上、status.replicas=0、相关Pod列表为空 | `observe_kubevirt_stopped` 复核 VM/rootdisk UID，VMI 不存在，返回 `KubeVirtStoppedObservation` | `cleanup` 再读所有清理目标，返回 Confirmed/Pending；`cleanup_recovery` 处理恢复引用 | Pod/VMI消失不等于失联节点已停止写盘；还需选定CSI/fencing证据及Resource权威回读 |
| readmit | `Apply`/`Scale` 等实际运行入口不能仅凭模型相位调用 | `Start` 经正式 start 子资源后 `wait_kubevirt_running`，不修改 virt-launcher | `AdmittedExecution::admit/recover` 要求有效Resource绑定；`ResourceClient::get_task_resource` / `release_task_resource` 是现有任务回读/释放边界 | 重新准入须有商业授权、Kueue准入及真实释放；停止预留不等于释放已售商业容量，不能统一调用release |

PR258 的 VM stop/restart 请求体使用 `gracePeriod:30`。这是已有 guest 关机参数，不是60秒保存宽限；两者必须由召回协调顺序区分，不在此擅自改参数。start/stop 返回409时该函数可返回 Ok，Stop 上层仍继续停止观察，因此不能把 HTTP 成功或409当资源已释放。接受预留重启到可连接的5分钟承诺须覆盖通知、保存、停止、盘隔离、重准入和启动；目前模型没有验证总期限。

## 字段唯一 writer 对照

| 字段/事实 | 已有权威或拟定边界 | 当前未闭合项 |
| --- | --- | --- |
| 非秘密配置 revision、`preemptionGraceSeconds`、`readOnlyRetentionSeconds` | PR248 Control 是唯一配置写入者 | executor/召回消费者须绑定实际采用 revision；不写第二份默认配置 |
| Resource request/claim/lease、商业资格、费用和召回 | Resource | 需要将召回操作关联到具体运行代次；模型generation不是新的公共DTO |
| Deployment `spec.replicas`、运行plan与代次、磁盘挂载 | Environment executor；PR258 `FIELD_MANAGER=labweaver-runtime-executor` | Kueue不得成为该字段第二writer；Pod准入字段需选定版本后逐项核定 |
| VM生命周期请求、VM/rootdisk身份 | Environment 经 KubeVirt 正式 start/stop/restart API；VMI与launcher由KubeVirt控制器管理 | 不直接改virt-launcher；尚无Kueue→VM正式集成契约 |
| 一次性Job和执行对象归属、清理观察 | task-execution的现有Kubernetes执行实现 | Kueue Job准入/suspend相关字段的writer及取消顺序尚待定，不能宣称已支持 |
| Kueue Workload准入条件、队列配额/公平 | 拟由所选Kueue集成控制器管理 | 版本、CRD、对象ownerReference、字段冲突策略尚未定；不是本轮可写契约 |
| Pod绑定到节点 | kube-scheduler | 不新增LabWeaver节点放置实现 |

[官方Deployment说明](https://kueue.sigs.k8s.io/docs/tasks/run/deployment/) 描述逐Pod准入，而非将整个Deployment视为一个Workload。该动态文档仅解释风险，不能替代固定Kueue版本的行为验证。

## 必须保留的反例

- Kueue在60秒前直接删除Pod：模型的受控stop顺序没有模拟外部控制器，仍须真实集群反例测试。
- Deployment在旧Pod消失后重建新UID的Pod，或VMI被控制器重建：一次消失观察不构成全程禁止运行保证。
- 节点失联、Pod/VMI对象消失但旧节点仍写RBD：目前没有独立存储fencing证据，保持未知，不能回收占用或重挂。
- Stop返回409、观察超时、权限拒绝或只读缓存滞后：需要权威身份回读，不能把请求结果当Confirmed。
- 通知重复、进程在60秒中重启、配置revision更换、取消与完成竞态：模型不持久化计时，也未证明幂等与跨进程恢复。
- 停止预留后继续保留售出容量，与可抢占任务释放实际占用是不同事实；不能以Kueue配额释放自动退款或释放预留。
- GPU拓扑/MIG/vGPU不可满足、启动超过5分钟：明确失败和补偿仍须真实Resource/Environment链路验证，不能静默fallback。

本轮没有新增生产API、DTO、store或部署入口，没有修改封存模型文件。下一步是先固定Kueue版本及正式集成对象，逐项核对上述字段writer，再在获准集群验证这些反例。模拟布尔隔离观察不能替代节点fencing。
