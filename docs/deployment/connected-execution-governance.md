# Connected 执行收敛治理

## 目的

package、部署、Resource replay 和 Release Gate 都可能占用共享集群、远程
BuildKit、数据库和真实运行时。没有执行边界时，一个失败会被重复部署放大为无限
测试周期。该文档是 `xtask` connected 入口的操作合同：先冻结身份，再取得租约，
按固定预算执行；任何无法确认的状态都阻断，而不是猜测成功或启动并行操作。

## 候选身份

候选至少绑定以下字段：

- `source_commit`；
- package manifest SHA-256（若操作消费 package）；
- deployment manifest SHA-256（Resource replay）；
- migration/configuration identity（由 Ansible report 绑定）；
- environment、operation、Run ID 和 testflight Run ID。

Resource replay 的额外 profile、authentication、deployment 和 package locator 也会
参与 configuration identity；`xtask` 对存在的 locator 计算内容哈希。因此同一路径原地
轮换凭据会形成新的候选，不能把新凭据伪装成旧尝试；账本仍按候选固定预算阻止无限重放。

Release Gate 只接受同一组身份。开发期可以复用 BuildKit/OCI 层缓存来减少编译，
但缓存不是验收证据，也不能把旧 digest、旧 Run ID 或旧 report 拼接到新候选中。
影响运行时、合同、部署、测试或证据生成的提交必须重新 package；仅文档变更应先
批量提交，随后由 Owner 判断是否需要重新部署和重放。

## 执行账本

connected 控制器必须设置：

```sh
export LABWEAVER_EXECUTION_LEDGER_ROOT=/var/lib/labweaver/execution-ledger
```

目录由 `xtask` 创建并固定为 `0700`。账本 JSON 只包含候选 hash、相对/受控
locator、时间、Run ID、尝试状态和稳定 diagnostic；`.lock` 使用 create-new
语义，避免 PID 文件在进程崩溃后造成误判。账本或锁无法读取时不自动删除，必须由
Owner 先做只读进程、Helm、Ansible 和集群状态检查。

## 固定预算

同一候选、同一环境按下表执行：

| 操作 | 最大尝试 | 失败后的动作 |
| --- | ---: | --- |
| package | 1 | 同一 source/config/release 只允许一次；失败后必须修复并产生新候选 |
| Resource replay | 1 | 记录 diagnostic；无新根因修复不得重放 |
| application reconcile | 2 | 第一次部署和一次幂等重放；第二次仍失败即 Blocked |
| 其他 deploy/reset | 1 | 只读核对，创建责任域 Issue |

每次尝试必须在同一账本中记录前置检查、唯一根因假设、实际状态变化、证据 locator、
终态和下一步。外部连接瞬时失败只允许一次有界只读复核；超时先读取原进程，不得
直接再开新 Run ID。

## 开发期缓存与发布 pin

不要通过取消 commit 绑定来“修复”重复部署。正确分层是：

1. 开发期在本地或受控 BuildKit 中复用经过校验的依赖层和组件缓存；
2. 使用新的组件/tree hash 做低成本静态和契约测试；
3. 进入 connected 验收前冻结一个完整候选，生成唯一 package/deployment manifest；
4. Release Gate 只消费该候选的 source commit、immutable digest、migration catalog、
   Run ID 和 connected evidence。

这样小的文档改动不会触发无意义的全量部署，而真正影响运行时的改动也不会被缓存
掩盖。任何“身份漂移”均为阻断，而非自动 fallback。

## 排障顺序

1. 只读检查账本、锁、远程进程、Helm operation 和目标 namespace；
2. 核对 package/deployment/catalog/configuration hash 与当前集群 readback；
3. 只修复一个已证实的根因，并先运行对应本地/契约/Ansible 负向测试；
4. 按剩余预算最多重试一次；
5. 若仍为同一 diagnostic，登记 `Blocked`，由 Owner 拆分责任域或安排基础设施维护。

禁止删除账本、切换旧 package、伪造成功 report、用 Fixture 替代 connected evidence，
或通过并行部署“碰运气”。
