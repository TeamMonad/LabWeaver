# 只读 CSI 绑定观察校验

`tools/check_storage_binding.py` 检查已有 PVC、PV 和 StorageClass JSON 快照之间的绑定一致性。它不连接集群、不写文件、不创建或修改 Kubernetes 对象，不读取管理员配置，也不选择 RBD profile、access mode 或回收策略。

在已获准的观察窗口取得三个对应 API 对象的 JSON 后运行：

```sh
python -B tools/check_storage_binding.py pvc.json pv.json storage-class.json
```

三个位置参数必须依次为一个 PVC、一个 PV 和一个 StorageClass，不能传 Kubernetes List。只使用非秘密 API 对象；不要传 Secret 或部署 Bundle。工具拒绝错误对象类型、重复 JSON 键和无法解析的输入。失败退出码为1，标准输出只含诊断码；成功退出码为0，只输出 `BINDING_CONSISTENT_ONLY`。

工具检查：

- 三个对象的名称、UID、resourceVersion 存在，且没有 deletionTimestamp 字段。
- PVC/PV 都声明 Bound；PVC volumeName 指向该 PV。
- PV claimRef 的名称、namespace 和 UID 与 PVC 完全相同，避免把重建的同名 PVC 当作原对象。
- PVC/PV 的 storageClassName 与给定 StorageClass 一致；CSI driver 与 provisioner 一致且存在 volumeHandle。
- PVC 的 accessModes 被 PV 包含，且两者 volumeMode 相同；省略 volumeMode 时按 Filesystem 比较。不把 ReadWriteOnce 或 ReadWriteOncePod 当作 fencing 证据。

它只比较输入，不能验证快照来源、真实性、采集时刻或跨对象一致读；resourceVersion 只检查存在，不跨对象比较大小。工具不会把成功写入 applied、配置 revision 或 Resource 状态。Control 仍是 #203 配置唯一写入者，Environment/Resource 的 #207 清理和租约栅栏仍是原有领域契约；本工具没有复制它们。

成功不证明 CSI/RBD 版本受支持、卷实际可访问、磁盘容量满足要求、Ceph 副本/故障域和健康、数据库同步持久性、NATS quorum、备份/RPO/RTO、附件已清理或失联节点已 fencing。更不代表可以重新挂载、删除、释放或重新准入。不能以它完成 #205 HA 验收。真实控制器、节点故障和持久性测试仍须在确定版本、硬件基线及专项许可后执行。

运行本切片的合成输入测试：

```sh
python -B -m unittest discover -s tests/storage_observation -v
```

测试不需要 pytest、Ansible、kubectl、Helm 或集群。假 driver `example.csi.invalid` 只用于验证对象引用逻辑，不表示任何支持的生产 provider。测试通过只证明输入校验行为。
