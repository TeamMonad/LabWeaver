# Connected 执行收敛治理

## 目的

package、部署、Resource replay 和 Release Gate 都可能占用共享集群、远程
BuildKit、数据库和真实运行时。没有执行边界时，一个失败会被重复部署放大为无限
测试周期。该文档是 `xtask` connected 入口的操作合同：先冻结身份，再按规则执行；
任何无法确认的状态都阻断，而不是猜测成功或启动并行操作。本窗口不依赖账本文件、
租约文件或替代预算机制，执行记录保存在验收 Issue 和受控交接记录中。

## 候选身份

候选至少绑定以下字段：

- `source_commit`；
- package manifest SHA-256（若操作消费 package）；
- deployment manifest SHA-256（Resource replay）；
- migration/configuration identity（由 Ansible report 绑定）；
- environment、operation、Run ID 和 testflight Run ID。

Resource replay 的额外 profile、authentication、deployment 和 package locator 也会
参与 configuration identity；`xtask` 对存在的 locator 计算内容哈希。因此同一路径原地
轮换凭据会形成新的候选，不能把新凭据伪装成旧尝试。候选预算由操作者按本合同执行，
不通过创建新账本、改名 operation 或生成新 Run ID 绕过限制。

Release Gate 只接受同一组身份。开发期可以复用 BuildKit/OCI 层缓存来减少编译，
但缓存不是验收证据，也不能把旧 digest、旧 Run ID 或旧 report 拼接到新候选中。
影响运行时、合同、部署、测试或证据生成的提交必须重新 package；仅文档变更应先
批量提交，随后由 Owner 判断是否需要重新部署和重放。

## 操作记录与互斥规则

每次 connected 操作开始前，操作者必须在验收 Issue 或受控交接记录中写入候选
`source_commit`、package/deployment/configuration/migration hash、镜像 digest 集合、
environment、Run ID、testflight Run ID 和执行意图。记录只包含 hash、locator、状态、
计数和 diagnostic，不包含 Secret、JWT、私钥或用户内容。

同一环境同一 operation 同时只能有一个执行实例。发现运行中的锁、无法确认的旧进程、
超时或未知终态时，只能做只读进程/Helm/Ansible/集群检查并转为 `Blocked`；本窗口不创建
账本文件、锁文件或替代预算机制，也不删除历史文件来恢复预算。

## 固定预算

同一候选、同一环境按下表执行；操作者同时限制同一 `operation + environment`
跨不同 commit、release label 或私有 locator 形成的新候选总数。这样修复一个问题
不会把 connected 测试变成无限的新候选循环。

| 操作 | 单候选最大尝试 | operation 总预算 | 失败后的动作 |
| --- | ---: | ---: | --- |
| package/release | 1 个冻结候选 | 1 个活动实例 | 失败、超时或身份不明直接保留现场并阻断；修复后必须重新冻结候选 |
| Resource replay | 1 | 3 | 只有已修复根因且有新观测才能重试；同一 operation 达到上限即阻断 |
| application reconcile | 2 | 3 | 首次部署和一次幂等重放；仍失败或达到总上限即阻断 |
| 其他 deploy/reset | 1 | 1 | 只读核对，创建责任域 Issue |

每次尝试必须记录前置检查、唯一根因假设、实际状态变化、证据 locator、终态和下一步。
operation 名称必须跨 release label 稳定；release label、source commit、package/deployment/
locator hash 进入候选键，而不是创建新的预算文件。外部连接瞬时失败只允许一次有界只读
复核；超时先读取原进程，不得直接再开新 Run ID。达到 operation 上限时停止写操作并登记
`Blocked`。

Resource replay 在 connected 写操作前还必须完成本地 preflight：deployment manifest 的
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

1. 只读检查受控交接记录、远程进程、Helm operation 和目标 namespace；
2. 核对 package/deployment/catalog/configuration hash 与当前集群 readback；
3. 只修复一个已证实的根因，并先运行对应本地/契约/Ansible 负向测试；
4. 按本合同剩余规则最多重试一次；
5. 若仍为同一 diagnostic，登记 `Blocked`，由 Owner 拆分责任域或安排基础设施维护。

禁止切换旧 package、伪造成功 report、用 Fixture 替代 connected evidence，或通过并行部署
“碰运气”。历史账本（若存在）只作审计，不是本窗口输入；本窗口不创建新的预算账本。

## 新验收窗口（2026-08-12）

部分历史记录损坏后，用户授权开启新的独立 #126 验收窗口并删除旧预算账本。
旧候选、旧镜像、旧报告和旧账本只作历史审计，不参与本窗口的 Release Gate；精确的
legacy ledger locator 已只读确认不存在。本窗口不创建账本或替代租约文件，而由 A 按
本合同保留候选身份、执行意图、状态和 diagnostic。任何身份漂移、超时、Provider 不可用、
清理残留或未知终态都保持 `Blocked`。

## #126 console 验收窗口

Container xterm 与 KubeVirt noVNC 只能在 #126 的同一冻结候选和 Run 中执行。A
负责候选冻结、操作记录和 connected 操作，D 保留独立 Verify；B 审查 console
安全边界和 Release Gate v3。开始任何写操作前，Owner 必须先记录只读维护审计，
明确授权新的验收环境和剩余规则预算。旧账本不作为输入，也不得通过新 operation 名称
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
frame 或控制器绝对路径。任一超时、未知旧进程、身份漂移或清理残留
都保持 `Blocked`，不得生成 `passed` 文件。
