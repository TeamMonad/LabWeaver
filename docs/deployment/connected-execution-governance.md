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

同一候选、同一环境按下表执行；账本同时限制同一 `operation + environment`
跨不同 commit、release label 或私有 locator 形成的新候选总数。这样修复一个问题
不会把 connected 测试变成无限的新候选循环。

| 操作 | 单候选最大尝试 | operation 总预算 | 失败后的动作 |
| --- | ---: | ---: | --- |
| package | 1 | 3 | 同一 source/config/release 只允许一次；失败后必须修复并产生新候选；第四个候选直接阻断 |
| Resource replay | 1 | 3 | 记录 diagnostic；无新根因修复不得重放；第四个候选直接阻断 |
| application reconcile | 2 | 3 | 第一次部署和一次幂等重放；第二次仍失败即 Blocked，跨候选累计第三次后停止 |
| 其他 deploy/reset | 1 | 1 | 只读核对，创建责任域 Issue |

每次尝试必须在同一账本中记录前置检查、唯一根因假设、实际状态变化、证据 locator、
终态和下一步。账本的 operation 名称必须跨 release label 稳定；release label、source
commit、package/deployment/locator hash 进入候选键，而不是创建新的预算文件。外部连接
瞬时失败只允许一次有界只读复核；超时先读取原进程，不得直接再开新 Run ID。operation
总预算耗尽返回 `LW_EXECUTION_OPERATION_BUDGET_EXHAUSTED`，必须停止写操作并登记
`Blocked`。

Resource replay 在取得账本租约前还必须完成本地 preflight：deployment manifest 的
`runId` 必须等于本次 `LABWEAVER_RUN_ID`，package/profile/auth locator 必须通过
当前提交的 identity 校验，storage-state 必须存在有效 cookie；所有带 expiry 的 cookie
都过期时返回 `LW_RESOURCE_REPLAY_AUTHENTICATION_EXPIRED`，不得消耗一次 connected
replay 预算。这样已知的身份错配和陈旧浏览器会话不会把昂贵的远程尝试留到 Ansible
或公共 API 阶段才暴露。

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

## 预算耗尽后的 Owner 续行决定（2026-08-03）

`resource replay` 与 `resource-application --infra` 在 demo 的原始预算已被多个不同
根因的失败耗尽：候选身份未绑定私有 locator、BFF 会话过期、replay driver 误用契约
未声明的 `statusUrl` 字段。旧账本文件完整保留为审计记录，不做任何删除或改写。
经 Owner（A）决定，根因修复提交之后启用一次性、有界的续行 operation：
`resource replay repair` 与 `resource-application-repair --infra`。它们各自拥有独立
的固定预算（单候选 1 次、operation 3 条），再次耗尽即阻断。该机制只允许在“每个
失败候选都有已证实且已修复的不同根因”时使用一次；同一根因的重复尝试仍然被候选
预算拒绝。Release Gate 不消费 repair operation 之外的旧证据，最终发布身份必须以
同一候选完整闭合 package、application、replay 与 report。

## #126 console 验收窗口

Container xterm 与 KubeVirt noVNC 只能在 #126 的同一冻结候选和 Run 中执行。A
负责候选冻结、账本租约和 connected 操作，D 保留独立 Verify；B 审查 console
安全边界和 Release Gate v3。开始任何写操作前，Owner 必须先记录只读维护审计，
明确授权新的验收环境和剩余预算。旧账本不得删除、改 root 或通过新 operation 名称
绕过。

验收需要六个隔离环境/每种 runtime，使用公开 API 创建专用 AccessGrant；expiry
case 创建短时 Grant，不能通过 renew 反向缩短。control-channel-loss 只能运行
`98-connected-console-control-loss.yml`：输入必须绑定隔离 namespace、Run label、
精确 Access Service Pod UID、configuration bundle hash 和两个 case environment ID。临时 Cilium policy 只拒绝
TCP 4222，并在 `always` 中删除和 readback；浏览器观察、策略应用、策略恢复均使用
私有 coordination marker，不进入报告。

Release Gate v3 只接受两个 `connected-console-evidence.v1` 文件。报告记录每个 case
的 Environment/Pod 或 VMI、Grant/Lease/capability/session revision、稳定 diagnostic、
artifact hash 与清理计数；不得记录 locator、Cookie、token、PTY transcript、VNC
frame 或控制器绝对路径。任一超时、未知旧进程、账本终态失败、身份漂移或清理残留
都保持 `Blocked`，不得生成 `passed` 文件。
