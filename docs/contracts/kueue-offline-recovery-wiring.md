# 离线恢复清理接线与 owner 建议对照

本轮在 `codex/211-recovery-http-wiring`、parent `4393c36ee1d3051f6489ffff520a6ae232a30533` 增加真实执行客户端的loopback HTTP测试，未接入Kueue控制循环。旧proposal及封存模型不变。原owner提供的 `/workspace/207-handoff/contracts-proposal-20261017/PROPOSAL.md` 在本环境不存在；下表基于本次交接中的完整建议、封存proposal和已读固定来源，不声称已读取该本地文件。

## 与两份封存proposal逐项对照

| owner建议 | 对照结论 | 当前证据和边界 |
| --- | --- | --- |
| 保持Kubernetes/KubeVirt/CDI 1.35.6/1.8.4/1.65.0 | 同意，与既定锁一致 | 仓库版本锁，不是集群实测 |
| Kueue 0.20.2、v1beta2 | 新候选，替代原“未锁定”的研究缺口；尚未改锁 | 固定tag的Workload CRD确有v1beta2；安装矩阵及实际集成仍需核定，不能称兼容已验收 |
| Rook/Ceph/CSI 1.20.9/20.2.4/3.17.1 | 候选差异，不是生产契约冲突 | 旧proposal的Rook1.19/CSI3.18.1只是研究候选；本轮核到1.20.9 cluster values的Ceph20.2.4，未核完CSI默认值/镜像摘要，不推导完整组合 |
| CSI Operator1.0.4、Addons0.14.0、单一部署者 | 同意单writer原则；版本仍为候选 | 固定Chart依赖含Operator1.0.4；所读rook-ceph values未直接找到3.17或0.14，仍需沿实际依赖和源码核实 |
| Resource具体模块投影原申请/运行代次；不另建服务/Lease，不与native集成双建Workload | 与原边界一致，补充了拟定实现入口 | 未实施Workload投影，也未验证Resource/Workload CAS和字段所有权 |
| 有状态Work关闭自动抢占；60秒保存→停止→隔离→持久化观察→Finished→释放Kueue占用 | 同意，并明确了原proposal的未覆盖风险 | 固定workload_controller源码的Finished/inactive更新路径移除队列/缓存；本轮不设active=false、不写Finished |
| Manual加正式start/stop/restart，Apply显式start | 条件同意，是拟定改变而非已有实现 | 原PR258有正式子资源入口；切换Always到Manual须改plan/校验器/回归，当前没有实施 |
| checkpoint固定desiredRevision+配置SHA+密文BundleSHA+providerSHA，恢复不换版 | 同意不使用aggregate revision替代不可变版本 | 需要原配置/业务checkpoint owner定稿，本测试不新增或复制该元组 |
| RBD Filesystem/ext4/RWO/Immediate/Delete，独立PVC保留到授权purge | 条件同意；不能现在启用 | 与旧proposal“不改回收策略”一致；按交接，当前计算退役仍会删PVC，须先盘生命周期解耦。冻结采集同节点约束仍由#197/#207落实 |
| 正常attachment退役与失联NetworkFence分别取证；不接受confirmed布尔；Environment只读 | 同意，与原fencing未知边界一致 | 本轮没有观察真实attachment或NetworkFence，没有节点fence/unfence操作 |
| 官方技术候选由owner核实，生产启用另行授权 | 同意 | 没有安装集群工具、操作集群或修改共享接口 |

固定来源：

- [Kueue v0.20.2 Workload CRD](https://github.com/kubernetes-sigs/kueue/blob/v0.20.2/config/components/crd/bases/kueue.x-k8s.io_workloads.yaml)
- [Kueue v0.20.2 Workload controller](https://github.com/kubernetes-sigs/kueue/blob/v0.20.2/pkg/controller/core/workload_controller.go)
- [Rook v1.20.9 Chart依赖](https://github.com/rook/rook/blob/v1.20.9/deploy/charts/rook-ceph/Chart.yaml)
- [Rook v1.20.9 operator values](https://github.com/rook/rook/blob/v1.20.9/deploy/charts/rook-ceph/values.yaml)
- [Rook v1.20.9 cluster values](https://github.com/rook/rook/blob/v1.20.9/deploy/charts/rook-ceph-cluster/values.yaml)

## 本次真实离线接线

`crates/task-execution/src/kubernetes.rs` 的生产 `capture_object_refs`、`cleanup_recovery`、`delete_owned` 和归属校验保持原样。新增测试使用原 `KubernetesApiClient::for_test` 与reqwest客户端，向随机loopback端口发送真实GET/DELETE JSON请求。测试服务器实现对象读回、UID/resourceVersion删除前置条件以及确定性的409竞争注入。HTTP只是本地夹具，不是对真实apiserver、TLS或RBAC的认证测试。

| 测试 | 实际接线与断言 |
| --- | --- |
| recovery_http_checkpoint_waits_for_actual_absence | capture得到原ExecutionObjectRef，JSON序列化/反序列化后交给原cleanup_recovery；DELETE成功但对象仍在时Pending，实际移除后Confirmed；重复恢复不重新submit |
| recovery_http_refreshes_same_uid_cas_after_conflict | 在第一次DELETE前改变同UID对象resourceVersion；客户端收到409后读回、刷新前置条件，再DELETE；核对两次请求的UID相同、版本分别rv-1/rv-2 |
| recovery_http_preserves_replacement_uid_after_conflict | 在DELETE前以新UID替换同名对象；409后返回IdentityConflict，仅发送一次DELETE，新对象保持不变 |
| recovery_http_rejects_another_attempt_before_delete | 同UID对象被标记成另一个attempt；恢复校验失败，未发送DELETE，不移除对象 |

运行新增测试：

```sh
cargo test -p task-execution --locked --offline recovery_http_
```

这不是新抽象模型，也没有用固定成功响应替代恢复客户端。为支持真实请求体分段到达，测试HTTP服务器按Content-Length读完请求体，并对记录的DELETE请求做断言。所有新增/修改的Rust代码都在既有 `#[cfg(test)]` 模块内。

## 未覆盖范围

JSON往返仅覆盖现有执行对象引用的恢复格式，不是业务数据库checkpoint/CAS事务或完整配置元组恢复。另一个attempt标签测试不等于已验证Resource lease/generation fencing。没有证明Kueue晚到准入、早失配额、Manual VM Apply显式start、60秒/5分钟跨进程计时、存储单写、NetworkFence陈旧/撤销、Resource账务或真实集群行为。后续可据已核候选继续做固定CRD/公开HTTP投影测试，但不能把本次四项恢复清理测试作为#211完成或生产启用许可。
