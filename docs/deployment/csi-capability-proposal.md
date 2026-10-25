# CSI 能力与只读绑定观察边界

组件版本、硬件要求、同步复制、健康准入和独立备份目标统一见[三节点持久化与高可用基线](high-availability.md)。版本真源是 `deploy/versions.lock.yml`，本文不维护第二份候选版本矩阵。下面保留只读检查器的能力边界；它不替代生产健康准入或硬件验收。

## 既定边界

标准部署至少三个独立节点、host 故障域、Ceph 三副本、RBD Work 盘。Environment 唯一管理运行代次和磁盘挂载，停止/抢占保盘。删除/欠费默认七天只读；`readOnlyRetentionSeconds=604800` 属于 Control 不可变 revision 的唯一配置真源。

`workspaceStorageClassName` / `workspaceAccessMode` 仍是权威 provider 输入；HA 选择 `ceph-rbd` / `ReadWriteOnce`，不新增平行开关。`cleanup_retention_seconds` 只控制清理证据保留，不是磁盘 TTL。

## 版本与能力核查

| 能力 | 支持边界 | 剩余验证 |
| --- | --- | --- |
| RBD Filesystem RWO | 三副本、host、Retain，版本与 CSI sidecar 全部固定在同一锁中 | 实际 I/O、旧节点失联与重新挂载的单写 fencing |
| RBD Block RWX | 上游矩阵不能外推为 LabWeaver 支持共享可写 Work 盘 | 本基线不启用 |
| RWOP | Kubernetes 稳定声明不能代替 Ceph-CSI 与业务契约支持 | 本基线不启用 |
| PV 回收与七天只读保留 | `Retain` 防止误回收，但不是业务 TTL 或只读保留实现 | #207 的授权、导出与删除行为 |
| 失联节点单写 fencing | Pod/VMI 或 VolumeAttachment 消失都不是旧节点无写入的证明 | #207 门禁与获准硬件故障验收 |

RGW 兼容仍是明确缺口；CNPG/JetStream 与独立备份配置的部署代码不等于三节点故障恢复通过。站点灾难目标已定，兑现待 #208 恢复验收。

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

在获准三节点环境验证旧节点失联、重挂单写、持久写恢复、近满/quorum 降级和独立备份。未完成前，不把绑定一致、渲染或合成输入测试当作 HA 硬件验收。
