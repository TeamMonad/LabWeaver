# 抢占时序的本地模型

这是 #211 的测试专用 PoC。运行：

```sh
python -B -m unittest discover -s tests/poc -p 'test_*.py' -v
```

模型用合成事件检查 Sprint5 的 60 秒保存宽限、当前运行对象的代次/UID，以及确认清理与磁盘隔离后才允许重新准入。`readmit` 只表示模型中的顺序检查通过，不分配资源、不创建下一个运行代次。任务的隔离观察表示执行资源已完成清理。测试输入中的 `confirmed` 和 `disk_isolated` 是模拟事实，没有从 API 读取。

模型只采用本次场景的 60 秒，不读取或替代管理员配置。生产消费者仍须从唯一配置 revision 取得策略。它不定义公共 DTO、Resource store、Kueue CRD、VM runStrategy 或 StorageClass，也没有被生产代码导入。

## 已核对的契约

- develop `cc39763ef3657833223a4e8dbfed9d6f2da7ac08` 的 Sprint5 方案第7节：保存宽限、停止后真实释放、失联节点 fencing。
- 同提交的 ADR0016 和 `crates/task-execution/src/admission.rs`：清理 Unknown 不能成为 Confirmed，不能释放 Resource。
- #203 / PR248 `1fafdc1e31f87b19c2c0240570faf53737bd9231` 的 `docs/contracts/platform-configuration.md`：Control 是配置 revision 唯一写入者；模型不是另一份配置。
- #207 / PR258 `550f5bb7ff69f2b6a1ccb5db936a6e2b2c70be47` 的 `ResourceWorkCleanupStatus`：生产集成必须进一步校验环境、project、owner、lease/revision、capacity binding、观察状态与 cleanup_complete。模型没有复制这些 DTO；代次/UID 只用于探索旧对象观察串入的风险。

## 后续集成仍需完成

| 责任 | 现有边界 | 未完成项 |
| --- | --- | --- |
| 商业资格、授权和释放 | Resource | 把模型场景映射到真实请求、租约与revision，验证商业准入 |
| 容器与VM生命周期、对象身份和盘隔离 | Environment | 选定正式控制入口，证明控制器不会重建未授权对象 |
| 一次性任务清理 | task-execution | 对接真实执行对象观察和清理确认 |
| 队列配额和公平性 | Kueue | 固定受支持版本和集成入口，完成最小集群PoC |
| 节点放置 | kube-scheduler | 验证真实拓扑与设备约束 |

没有锁定 Kueue/KubeVirt 版本，没有验证 Deployment/VMI 重建行为，也没有模拟 Kubernetes 控制器。不能用这些测试关闭 #211 或开始声称 S5-16 集成完成。需要在工具和版本基线具备后增加真实暂停、通知、停止、对象消失、存储 fencing 与重新准入的观察测试。不得直接修改 virt-launcher 或静默使用另一调度路径。

## 审计修复：初始状态与身份类型

Recall 构造器只接收 runtime、正整数 generation 和非空白字符串 uid。phase、notified_at、last_at 均不能由构造参数注入；普通属性赋值也被拒绝。状态只通过 apply 的受检路径推进。事件在比较身份前执行同样的严格类型检查，拒绝浮点代次、布尔代次及非字符串 UID。

这保护模型的正常 Python API 使用，不是对运行同进程恶意 Python 代码的隔离；显式 object.__setattr__ 等反射操作不在模型保证内。清理与隔离仍由合成观察提供，未增加任何真实控制器或集群验证。
