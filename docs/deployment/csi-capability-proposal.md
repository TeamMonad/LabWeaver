# 三节点存储版本与 CSI 观察矩阵（proposal）

这是 #205 的待验证能力矩阵，不是生产版本锁或支持声明。代码基线为 develop `cc39763ef3657833223a4e8dbfed9d6f2da7ac08`；只读检查器已封存在 `913f706fc9eff48bc8821bbe1123c213c0fe41dd`，本轮不修改该检查器或公共配置。

## 既定边界

标准部署至少三个独立节点、host故障域、Ceph三副本、RBD Work盘已确定，不重新选架构。Environment唯一管理运行代次和磁盘挂载，停止/抢占保盘。删除/欠费默认七天只读；PR248已有 `readOnlyRetentionSeconds=604800`，Control不可变revision是唯一配置真源。

[配置owner的交接](https://github.com/TeamMonad/LabWeaver/issues/203#issuecomment-6188092719) 确认现有 `workspaceStorageClassName` / `workspaceAccessMode` 是权威provider输入，当前仅接受RWO/RWX；不新增平行开关。`cleanup_retention_seconds` 只控制清理证据保留，不是磁盘TTL。消费者采用哪个revision及如何报告观察，仍需原owner串行协调。

## 版本与能力核查

| 组件/能力 | 已知来源 | 本轮结论与剩余验证 |
| --- | --- | --- |
| Kubernetes v1.35.6 | 仓库 `deploy/versions.lock.yml` | 已锁；本地没有运行该集群 |
| KubeVirt v1.8.4 / CDI v1.65.0 | 同一版本锁 | 已锁；CDI导入/克隆、VM盘重启及容量放大须在实际CSI组合验证 |
| Rook | 未在当前版本锁固定；[v1.19前提](https://rook.io/docs/rook/v1.19/Getting-Started/Prerequisites/prerequisites/)列Kubernetes v1.30–v1.35 | v1.19系列可列为研究候选；具体patch、Ceph/CSI打包组合、镜像摘要和节点内核未知，不据此修改锁文件 |
| Ceph | 三副本、host故障域及RBD方向已定，具体版本未锁 | 需与Rook正式支持组合、磁盘/网络容量、quorum及近满拒写策略一起定稿 |
| Ceph-CSI v3.18.1 | [该tag的README](https://github.com/ceph/ceph-csi/blob/v3.18.1/README.md)列测试Kubernetes v1.34/v1.35/v1.36 | 仅为候选，不能从minor交集推导完整Rook/Ceph/CDI组合通过；未获取镜像或安装 |
| RBD Filesystem RWO | 同一Ceph-CSI矩阵列GA | 本地绑定检查不能证明单Pod独占或旧节点停止写盘，仍需fencing |
| RBD Block RWX | 同一Ceph-CSI矩阵列GA | 不能推导共享可写Filesystem支持，也不能把Work盘改成多writer；不更改现有accessMode |
| RWOP | [Kubernetes访问模式说明](https://kubernetes.io/docs/concepts/storage/persistent-volumes/#access-modes)列v1.29起稳定且要求CSI及sidecar最低版本；Ceph-CSI v3.18.1矩阵仍列Alpha | 上游各层声明不能互相代替。当前LabWeaver契约还不接受RWOP；须逐层确认支持状态、固定sidecar版本并由owner修改唯一真源，不能以Kubernetes稳定就宣称已支持 |
| PV回收与七天只读保留 | 业务规则已定；具体回收及CSI生命周期未定 | `Retain`或`Delete`字段不等于业务磁盘TTL，也不证明旧节点fencing；本轮不选择或改写策略 |
| 失联节点单写fencing | Sprint5要求已定，具体机制/证据未定 | Pod/VMI消失、Bound、VolumeAttachment消失或单一accessMode都不能独立证明旧节点无写入；明确证据来源、身份和新挂载前的门禁后再实施 |

动态网页和tag文档只供方案核对，不能替代部署时固定版本/摘要及硬件验证。RGW版本/保留/Harbor兼容、CloudNativePG同步持久性、NATS三副本、独立备份与站点RPO/RTO也仍未由本矩阵验证。

## 合成快照的诊断与反例

| 输入情形 | 当前检查器结果 | 能说明什么 |
| --- | --- | --- |
| claimRef指向同名不同UID的PVC | `CLAIM_REFERENCE_MISMATCH` | 快照引用不一致；不允许当作旧PVC |
| PV与PVC/StorageClass名称不符 | `STORAGE_CLASS_MISMATCH` | 对象绑定链不一致 |
| CSI driver与StorageClass provisioner不符 | `CSI_DRIVER_MISMATCH` | provider身份声明不一致 |
| PVC或PV Pending/未知状态 | `BINDING_NOT_BOUND` | 不能认定快照中已绑定 |
| CSI三对象一致，但driver根本未安装 | `BINDING_CONSISTENT_ONLY` | 检查器没有查询CSIDriver、节点插件或支持版本 |
| Filesystem RWX三对象一致，但RBD实际不支持该能力 | `BINDING_CONSISTENT_ONLY` | 输入一致不等于能力可用，矩阵及真实I/O验证仍必要 |
| reclaimPolicy=Delete且绑定一致 | `BINDING_CONSISTENT_ONLY` | 绑定一致不保证七天保留；它不是删除/回收许可 |
| 对象resourceVersion存在但采集已过时 | `BINDING_CONSISTENT_ONLY` | 跨对象采集时间、API server来源和观察新鲜度未知 |

新增 `tests/storage_observation/test_diagnostic_boundary.py` 用合成输入固定最后四种“成功仍不能认证安全”的边界，没有改生产准入逻辑。运行新增测试：

```sh
python -B -m unittest discover -s tests/storage_observation -p test_diagnostic_boundary.py -v
```

下一步先由#203/#207/#205 owner固定Rook/Ceph/CSI组合、accessMode/回收与fencing证据，绑定配置采用revision；再在获准三节点环境验证旧节点失联、重挂单写、持久写恢复、近满/quorum降级和独立备份。未完成前，不把此矩阵或合成快照当作HA验收。
