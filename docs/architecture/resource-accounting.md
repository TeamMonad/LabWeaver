# 资源占用与费用

本文规定 Resource 的业务边界。公共字段以 contracts 及生成 Schema 为准；实施进度与实际测试结果在对应 Issue/PR 中记录。

Resource 保存批准的资源规格、容量保留、租约、用量、费率版本、项目 Credits 账户、预占、费用和不可变账目。Environment 保存运行对象及其观察结果，并通过可重放的生命周期通知向 Resource 报告占用变化。请求停止不代表停止完成，租约过期也不代表设备已归还。

本轮一次性任务的执行方是 Evaluation 的已批准 Job 路径。资源申请使用独立 TaskRunId；执行方将其与实际持久化任务、租约和运行尝试绑定，并显式确认领取与释放。Resource 不为任务生成虚假的 Environment，也不在没有执行方确认时直接标记交接完成。

## 一次性任务的审批、取消和恢复

Agent 生成与 Evaluation 执行共享 `TaskResourceLifecycle`。同一次尝试在恢复时复用 `TaskRunId`、申请、claim 和租约；业务重试在确认旧尝试清理后创建新的 `TaskRunId`。Resource 的普通申请重试接口拒绝 Task 目标，防止旧授权复活。业务领域保留历史尝试及失败原因。

Resource 在首次创建事务中用数据库时钟保存批准等待期限。内部创建字段 `approvalTimeoutMilliseconds` 为 1 至 3,600,000 毫秒；同 TaskRunId 的重放和配置变化不延长期限。Resource 的后台、任务查询和批准事务检查这个期限。期限先到且申请仍为 `Reviewing` 时，Resource 原子取消申请、保存 `LW_TASK_RESOURCE_APPROVAL_TIMEOUT` 并发出事件。执行方重启或丢失超时回复后仍读取这个原诊断。批准事务先提交时，执行方继续同一次尝试；迟到批准不能复活已取消申请。

批准的 `validUntil` 与等待期限不同。交接前批准失效时，Resource 原子释放 Reserved/Provisioning 的容量和 Allocating 租约，保存 `LW_RESOURCE_APPROVAL_EXPIRED`。已交接的租约过期时，Resource 保存 `LW_RESOURCE_TASK_LEASE_EXPIRED`，保留容量占用；Agent/Evaluation 停止尝试并通过现有清理路径确认对象消失后释放。恢复记录已有执行绑定时，即使提交回复丢失，也须先查询和清理对象。

用户取消与 worker 租约丢失分开处理。用户取消要求当前 worker 完成清理；租约丢失停止旧 worker 的处理，由新 worker 恢复同一次尝试。旧 worker 不得把失去执行权解释成用户取消并释放新 worker 正在使用的授权。执行方等待有界 claim/ack 响应后处理用户取消；响应不明时保留任务并查询原记录。

内部任务接口继续要求已配置的 Agent/Evaluation 服务身份和对应 `resource.task.*` 权限：

| 接口 | 当前语义 |
| --- | --- |
| `GET /internal/v1/task-resources/{taskRunId}/request` | 查询 Resource 权威申请状态；事件到达不替代查询 |
| `POST /internal/v1/task-resources/{taskRunId}/claim` | 领取原批准；重复领取不生成新的 claim |
| `POST /internal/v1/task-resources/{taskRunId}/ack` | 用 claim/lease revision 和 namespace 确认交接；相同交接重放不延长租约或再发事件 |
| `GET /internal/v1/task-resources/{taskRunId}` | 查询完整 claim/租约，供丢响应与恢复处理 |
| `POST /internal/v1/task-resources/{taskRunId}/cancel` | 只取消指定 revision 的待审批申请；批准先提交则返回冲突 |
| `POST /internal/v1/task-resources/{taskRunId}/release` | 执行方确认清理后释放；相同 revision 操作重放返回已完成状态，不重复释放或发事件 |

ack 重放只接受相同 namespace、有效 Active 租约和当前或原交接 revision。release 重放只接受原释放操作的两个 revision。其他 namespace、旧尝试、过期租约和不匹配的 revision 返回冲突。审批等待失败不写成学生程序零分；实际程序结果继续由 Evaluation 确定。

项目 Credits 账户与原子预占由 #209 交付；估价、用量/限额与不确定用量期限由 #210 交付。#200 的自动准入需接这两个正式接口。现有预算提醒不能被描述为 Credits 授权，本次修复保留现有人工审批路径。

## 分配与释放

新建 Work 从项目内可用的发布版本和资源规格发起申请，Environment ID 随本次幂等申请分配，用户无需输入内部标识。审批通过后，Resource 向 Environment 交接同一目标，Environment 才创建实际环境。申请不能要求一个已经运行的 Work，也不能预建绕过资源授权的占位运行对象。续期和回收继续绑定既有租约与环境。

GPU 目录明确区分独占、容器时间片和 VM vGPU。目录规格绑定设备插件资源名或 KubeVirt 设备规格，审批之后不能按其他可用资源静默替换。容量保留必须和分配状态在同一事务中更新，并在真实释放被确认后结束，避免并发审批超配。

容器时间片每个工作负载使用一个共享份额；份额不代表独占显存或比例算力。VM vGPU 使用已配置的 mediated device 规格。目录、观测或设备条件不足时返回明确诊断，不能回退到 Mock 或另一种 GPU 模式。

同一 Provider 的独占与时间片目录项必须绑定不同的扩展资源名。部署者需在设备插件中配置对应名称；仅给工作负载附加模式标签不能区分两种实际调度。目录拒绝跨模式的资源名冲突，Environment 原样申请批准的资源名，真实节点和 mdev 配置仍需在部署环境验证。

Resource 通过只读集群观测刷新可用容量，并设置观测有效期。观测必须覆盖所选资源的节点容量与外部工作负载占用；自身工作负载只有与持久分配记录匹配后才能从外部占用中排除，不能仅相信标签。容量检查在扣除外部占用后，再扣除尚未确认释放的本平台分配。查询失败、数据不完整或观测过期时停止新增分配。

课程 Experiment 使用教师已批准发布版本中的 CPU、内存、存储和可选 GPU 规格。Environment 从同项目、同发布版本的权威发布快照取得规格，在创建实验前通过受限服务接口向 Resource 登记归属、规格和执行绑定。Resource 保存这一不可变授权，拒绝同实例下不同归属或规格的重试；浏览器不能自行登记授权或指定 Kubernetes 资源名。GPU 在活动目录及实际容量观测中解析并预留；相同实例重试幂等，未知类别、容量耗尽或观测过期时明确失败。教学环境直接以 Environment 实例为归属，不创建虚假的 Work 申请或租约。GPU 预留计入同一物理分配绑定的容量扣减，并在容量观测中作为平台自身占用排除，不与 Work 租约重复计数。实际删除后才释放预留；停止仍保留，以支持重启。已释放实例的迟到申请不能重新占用容量。

内部路由 `POST /internal/v1/environment-resource-reservations` 需要服务账户权限 `resource.environment.resolve`，`POST /internal/v1/environment-resource-reservations/release` 需要 `resource.environment.release`；两者都要求调用方为已配置的 Environment 服务客户端，浏览器委托路径不可访问。内部用量报告继续需要 `resource.usage.record`，并检查对应 Environment 或一次性任务的执行方身份。

## 用量

计算资源从环境可用时开始计量，正常停止以执行后端观察到的实际停止时间结束。排队和镜像准备不算作环境可用时间。异常清理或无法确认停止时暂停自动结算，保留待核实状态，不能据此声称资源免费或已释放。

Experiment 与 Work 共享同一持久化生命周期计量实现。CPU、内存、存储和 GPU 都进入 Resource 的用量、费率分段、结算和调整流程；一次性 Agent/Evaluation 任务继续绑定真实申请及租约。用量目标明确区分 `resource_request`（真实申请与租约）和 `experiment_environment`（教学环境），Resource 从已保存的授权确认项目与课程归属。GPU 类别、模式及数量取实际分配快照，不能按后来修改的目录重新解释历史用量。

历史记录通过一次性迁移保存原事件、区间、授权快照与费用关联。缺失可信边界或数量时保留待核实状态，不补造零值，也不以新的计费路径追溯生成没有依据的费用。

存储占用和计算占用分开；停止计算后保留的存储继续按存储规则计量，直到实际删除或释放。每条占用记录绑定资源归属、时间区间和事件身份。重复投递不能重复计费；乱序通知不能回滚已确认的生命周期。未知用量必须显式表示，不能填零。项目用量通过 `GET /api/v1/projects/{projectId}/usage` 分页查询，响应包含 `items`、`page`、`pageSize` 和 `hasMore`；费用以 `usageRecordId` 关联对应环境或任务及实际区间，历史费用不能代替当前任务的结算状态。

## 费率与费用

费率必须声明计量维度、价格对应的基本单位数量、生效时间和费用单位。使用固定精度十进制及有检查的算术；每条费用明细绑定实际使用的费率版本。计价区间跨越费率变更时按生效边界分段，旧费用不能因更新费率而被重算覆盖。

新增后续费率可原子结束上一条未设结束时间的费率区间；历史价格和版本不变，不允许因此改变已经结算的用量。其他重叠区间明确拒绝。

管理员可在费用页面为开放的费率安排未来截止时间，无须发布替代价格。截止时间必须晚于费率生效时间和当前时间，不能截断已经结算的用量。结束操作与结算共享同一费率协调边界，不修改历史价格、版本或费用快照，也不强制停止环境。截止后没有匹配费率的用量保持待结算；临时试用应先确认资源已释放、已知用量完成结算，再安排结束。结果不明时先查询原费率，重试复用原请求与幂等键；页面显示“已安排结束”和“已结束”的区别。

没有匹配费率时显示“未配置计价”，不显示免费。数值溢出、计量冲突或配置不完整时停止自动结算并返回诊断。管理员调整单独记录原因与关联费用，不覆盖原始用量或历史明细。

页面按相同生效区间显示费率，多个同时生效的匹配项是配置冲突，不能选择最大 revision 掩盖重叠。费用配置错误与资源准入分别处理；仅计算费用的旧路径仍由目录、容量和审批规则决定；需要费用上限预占的消费入口还要求可计算的 CREDITS 费率和足够余额。

预算只提供提醒，不作为账户准入。项目 Credits 只对显式 `metered-usage` 预占扣收，费用、账户与账目在同一事务提交。超过剩余预占的尾部成本由平台承担。没有绑定预占的路径仍只计算费用。Resource 已提供真实费率估价和估价预占接口。全消费入口的强制准入和续期仍由 #210 接入，平台不接真实支付。用户页面分别显示计算、存储与 GPU 费用，以及未计价和待核实项目，不能把部分已结算费用展示成完整账单。

账户字段、管理员双边操作、未知标记、真实 API 和恢复边界见 [项目 Credits](../contracts/project-credits.md)。发起停止不释放预占，确认实际回收后才释放未用金额。

计量保存毫秒分子与显式quantityDivisor，费率继续声明每秒基础单位。估价、结算和费用契约校验共用精确整数舍入；计算与存储可以独立覆盖不同区间。接口及重放语义见 [项目 Credits](../contracts/project-credits.md#计算与存储估价)。

未知用量的剩余预占在首次未知事实1小时后释放，未知记录仍可查询。迟到结算直接按平台承担处理，已知结算事务同样检查期限，不能靠后台循环顺序决定谁付款。该资金动作不视为实际资源释放。

Resource 内部分工保持具体函数和唯一真源：`pricing` 查询版本化费率并计算用量费用，`costing` 提供估价与价格同意，`admission` 在授权事务中预占上限，`credits` 保存账户、不可变账目和到期处理。Resource store 协调同一个 SQLx 事务，预占与执行授权一起提交，计算费用与扣收也一起提交；分文件不增加远端事务或第二套状态。

Outbox 与账务维护使用独立循环，仍在同一个 Resource 进程。真实 PG/NATS 故障场景中，Broker 接收事件但不回 ACK 时，资金到期处理继续；未确认的 Outbox 事件仍保持未发布。没有新增 Billing 服务、消息框架或虚假发布成功。

## 模型调用的计量

Agent 的所有 Claude/Anthropic Messages 请求必须先经过 Agent model gateway。Worker 不接收提供商
URL 或长期 API key；每个 run/attempt 取得限定 project、run、attempt 且短期过期的 HMAC capability。
任意副本除验证签名和过期时间外，还从 Agent PostgreSQL 检查同一 run/attempt 的运行状态和租约；
取消、完成或租约失效后不能继续调用。Gateway 转发前调用 Resource model admission，余额不足或
缺少四类完整费率配置时返回 Anthropic error 和 `LW_MODEL_ADMISSION_REJECTED`，不接触上游。

模型费率是四个独立的 CREDITS 单价：input、output、cache-read、cache-write，并各自使用配置的
token 基数；cache read 和 cache write 不折算成 input，也不从 USD 推导。准入绑定单次最大预占和
四类费率快照，新费率不会改写已接受调用。Gateway 在外部副作用前持久化 Unknown 观察与调用 fence：
进程崩溃或请求取消也不会留下无期限 Pending。非流式、SSE 正常结束或两端中断时，有已观察的 usage
就按该用量结算；没有 usage 则保持 Unknown，不按零结算。首次未知一小时后释放剩余预占，迟到费用
由平台承担。`x-labweaver-call-id`（或 provider `idempotency-key`）绑定签名能力范围；相同调用只预占一次，
返回 `LW_MODEL_CALL_OUTCOME_UNKNOWN` 的重试只能查询原 hold，不能重放上游副作用。

## BuildKit 构建

Dockerfile 构建在 BuildKit 调用前以真实 BuildRequest/attempt 预占最大构建时长。计价单位为 builder 专属 `build_second`；未执行的 CPU/内存上限不是用量。Resource 校验 Agent 的真实命令与租约，在自身事务内提交预占和执行 grant，Agent 再在本地事务核对取消与执行 fence。跨进程请求不被描述为一个远端原子事务。

起止时间来自身份唯一匹配的 BuildKit history，仍使用统一版本分段、精确算术、未知期限和尾部平台承担。结果未知时只查询原操作；构建费用查询独立于会被清理或替换的工作流 fence。接口、精度和恢复边界见 [BuildKit 构建费用](../contracts/project-credits.md#buildkit-构建费用)。

## 原 Environment lease 的资金事务

Environment lease 激活与续期已经通过 Resource 的原 request、claim、approval 和同一 hold 提交资金覆盖。初次截止取原批准时长及 validUntil；续期累计报价先使用原额度，只追加不足部分。原 lease revision、幂等键和不可变报价窗口共同防止重复预占。覆盖到期而最终用量或释放尚未确认，资金进入首次一小时核实；未激活预占在原批准到期后结束冻结，实际容量回收仍由既有 owner 完成。

ResourceEnvironmentHandoff 的用途取可信发布快照中的 class，资金查询只返回原授权与账户事实。Environment 的新 compute 入口由 #207 刷新该 grant 并核原 runtime receipt；Work/Experiment 接收与真实执行仍由同一 Environment 生命周期实现。已有教学 reservation 路径继续代表其原实现；新的 funded handoff 只消费原 claim/lease，不再次调用该独立 reservation。接口、锁顺序和剩余范围见 [项目 Credits](../contracts/project-credits.md#原-environment-lease-的资金覆盖)。
