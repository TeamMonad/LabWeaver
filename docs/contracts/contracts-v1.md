# 公共契约

crates/contracts 是领域类型、校验、HTTP/SSE、NATS 和 Schema 的语义来源。OpenAPI、JSON Schema 和 Web SDK 从同一来源生成，不手工维护平行类型。文件名和现有 API 版本不代表承诺保留旧消费者；v3 按已批准范围同步更新仓库内调用方。

## 归属与权限

Project 是 Work、资源申请和费用的必选归属，Course 为可选教学关联。教学课程自身的接口仍保留课程语义。Project 授权只依赖相应项目成员关系；Environment 授权使用所属 Project、可选 Course、实例和运行代次。

内部身份来自验证过的服务 JWT，不来自 actor、role 或 principal 请求头。受限服务代表用户调用时，Access 仍检查真实会话、成员及资源范围。

## 生命周期和结果

环境、评测和 Agent 任务保留各自权威状态。长任务返回可查询操作及真实阶段，取消和终态由实际 Owner 推进。刷新、重试、重复事件和进程重启通过幂等与代次条件更新处理。

资源目标区分长期环境和一次性任务；目标身份由实际 Owner 绑定项目后申请资源，不凭空创建长期 Work。GPU class 由资源目录解析成独占、时间片或 vGPU，不能由调用者篡改成另一种模式。

费用包含显式单位、费率版本、计量区间与调整原因。未知计量或缺少费率不能输出已结算的零费用。冻结文件与实时 Probe 分开表示，基础设施错误不当作学生失败，LLM 不改变确定性成绩。

## 任务进度查询

`GET /api/v1/projects/{projectId}/agent-runs/{runId}/task-progress` 使用原 AgentRun 读取权限。`GET /api/v1/projects/{projectId}/me/evaluation-tasks/{runId}` 使用学生自己的评测读取权限，并在 Evaluation 检查项目与 actor；其他人的 run 返回 404。两者只读，不授予新的取消或重试权限。

学生从 Freeze 接受响应取得 submissionId 后，可用 `GET /api/v1/projects/{projectId}/me/evaluation-tasks/submissions/{submissionId}` 查询最近创建的所属评测 Run，返回同一进度 DTO。异步任务尚未创建时返回 404；其他项目或其他 actor 的提交也返回 404。该查询不创建任务，重试同一 URI 即可刷新。

响应以 `current` 返回每个 track/step 的当前尝试，以 `history` 分页返回旧尝试。`page` 从 1 开始，`pageSize` 为 1 至 100，默认 25；`hasMore` 仅描述历史页。换页不能把旧失败显示为当前进度。Agent 的同一业务尝试可能包含多个执行代次，旧代次留在历史；Evaluation 未领取的步骤以 attempt 0 表示，尚无 TaskRunId。

Work 配置脚本执行沿用现有环境和 Agent 已保存的执行意图；进度显示该阶段，但不补造新的 sandbox TaskRunId。生成阶段的旧执行代次保留在历史。

`phase` 来自业务检查点和 Resource 权威事实，区分等待批准、分配、执行和待清理。`diagnosticCode` 保留当前阻塞或失败原因；`taskRunId` 和 `resourceRequestId` 关联真实记录。`nextSteps` 只列已有操作 ID，客户端仍按项目、角色及服务端状态检查使用；没有公共入口的 Evaluation 取消/重试不会以可用按钮列出。

进度读取不返回命令、凭据、评分输入、分数或 Kubernetes 对象。Resource 绑定缺失或暂时不可用返回 503 / `LW_TASK_PROGRESS_RESOURCE_UNAVAILABLE`，不返回旧失败作为新进度。读取时 owner 改变则有界重读，仍不稳定时返回 409 / `LW_TASK_PROGRESS_CHANGED`。响应使用 `Cache-Control: no-store`，事件用于提示刷新，不能替代权威查询。

这两个接口读取指定任务，不新增全项目任务聚合。项目 Agent 列表仍只支持 `page`/`pageSize`；Resource 项目申请列表仍为按调用者可见范围返回的数组。客户端不能把第一页或可见数组当作全项目完整任务数。

## 格式与错误

标识使用现有领域 UUID 类型，revision 和业务序列继续使用既有约束。输入拒绝冲突、未知字段、路径越界和非法数值。错误使用可读 Problem Details 与稳定 diagnostic，保留足够上下文但不记录 Secret 或用户正文。

内容摘要和镜像 digest 用于真实输入与产物完整性；运行时不要求开发证明包或固定镜像数量。生成与校验命令以当前 xtask 和 Web scripts 为准，修改公共契约必须重新生成并检查所有消费者。

材料上传的查询、取消、恢复和限额见[材料上传与内容身份](material-uploads.md)。学生冻结提交继续使用[Collector 契约](submission-collector-v1.md)。
