# 材料上传与内容身份

Control 保存材料上传会话和材料包。Evaluation 保存学生冻结提交。两者的文件范围、对象版本和保留策略独立。
材料包的 `files[].sha256` 和冻结提交的 `files[].sha256` 均为实际文件字节的 SHA-256。路径或文件大小不能代替内容身份。

## 创建、查询和恢复

1. 在发送创建请求前，保存本次操作的 `Idempotency-Key`。
2. 调用 `POST /api/v1/projects/{projectId}/problem-package-uploads`。保存返回的上传会话 ID。
3. 按 `uploadTargets` 上传文件。每个 PUT 必须包含返回的 `requiredHeaders`，其中签名的 `Content-Length` 必须等于该文件声明的 `sizeBytes`。
4. 响应丢失或重新打开页面后，调用 `GET /api/v1/projects/{projectId}/problem-package-uploads/{uploadId}`。
5. 对 `pending` 会话，只上传查询返回的缺失文件。`uploadedPaths` 表示对象的大小和媒体类型匹配，完成操作还会验证实际内容。
6. 保存完成操作的幂等键及首次 `If-Match`，再调用 `POST /api/v1/projects/{projectId}/problem-package-uploads/{uploadId}/complete`。
7. 完成响应丢失后，先查询同一会话。`completedPackage` 是已完成的同一材料包。
8. 对 `completing` 会话，使用查询返回的 `completionIdempotencyKey` 和 `completionExpectedRevision` 恢复原请求。

创建响应丢失时，用原创建幂等键重发相同请求。不要创建第二个意图。
查询不延长会话到期时间。材料使用条件 PUT，已存在的对象不能覆盖。PUT 返回 412 时先查询会话。
材料恢复以文件为单位。平台镜像的大文件继续使用既有 multipart 接口，按服务端已上传的分片恢复。

所有上传会话接口都需要所属项目的 `problem_package:write` 权限。查询包含 PUT 授权，材料只读权限不能使用该接口。
已完成材料包的独立读取接口继续使用 `problem_package:read`。跨项目的上传会话查询返回 404。

## 完成、取消和过期

完成操作逐个读取确切对象版本，用固定 1 MiB 缓冲区写入临时文件，并在同一次读取中计算 SHA-256。
每个 Control 服务实例最多同时验证两个材料包。服务克隆共享这个上限。等待验证名额期间不占用完成租约。
Control 持久化对象版本、内容 hash 和材料包后，才返回成功。相同完成请求的重放返回同一材料包。
已完成会话即使收到新的完成幂等键，也只返回原结果。请求身份变化仍返回冲突。

对象暂时不可用时，会话保留原完成意图和已验证版本。原请求可重新取得租约。
`LW_PACKAGE_HASH_MISMATCH` 或 `LW_PACKAGE_OBJECT_VERIFICATION_FAILED` 表示校验失败，服务不会发布部分材料包。
发生终态失败后，修正输入并创建新的上传会话。

1. 取消前，查询会话的当前 revision。
2. 调用 `POST /api/v1/projects/{projectId}/problem-package-uploads/{uploadId}/cancel`，携带当前 `If-Match` 和稳定的取消幂等键。
3. 停止客户端正在发送的 PUT。服务端将会话改为 `cancelled`，阻止完成请求发布材料包。

取消不能撤回已经签发的 S3 URL。服务端不再签发新 URL。原授权到期后，Control 才枚举和删除该会话的确切孤立版本。
Control 每五分钟复查终态会话，处理授权到期前开始、到期后结束的 PUT。清理失败沿用现有版本清理账本重试。
取消结果不表示对象已经删除。上传不创建计算资源或付费任务，但取消前后的临时对象仍占用对象存储。

未完成上传到期后进入 `expired`。已受理的完成操作可在有效完成租约内继续。
会话和完成租约均已过期时，服务端停止该完成代次并清理孤立版本。旧请求不能复活过期或取消的会话。
`LW_PACKAGE_UPLOAD_EXPIRED` 要求重新创建上传。`LW_PACKAGE_UPLOAD_CANCELLED` 表示原操作已取消。

## 限额与可见范围

| 输入 | 当前限制来源 |
| --- | --- |
| 材料文件 | 对象存储 `maxObjectBytes`，以及整个材料包的 `maxPackageBytes` |
| 材料包 | Control `maxPackageFiles` 和 `maxPackageBytes`。示例配置为 1000 个文件、256 MiB |
| 平台镜像上传归档 | 5,000,000,000 字节。multipart 分片为 64 MiB |
| 展开的 OCI 或磁盘内容 | Agent 既有 8 GiB 预算，与压缩归档大小分别计算 |
| qcow2 虚拟容量 | 审核的 VM 产品容量，另受平台 VM 容量策略限制。它不是上传或展开字节数 |
| 学生冻结输入 | manifest 限额和 Collector 服务限额同时生效。默认原始文件 64 MiB、规范归档 96 MiB、10,000 个文件 |

本变更不提高既有上传、展开或虚拟容量限额。空材料文件、重复路径、路径穿越和超限材料会失败。
Collector 只收集 manifest 授权的文件，拒绝符号链接和路径逃逸，不把邻近教师材料或隐藏规则加入学生归档。
Collector 在既有预检和冻结读取时生成逐文件 hash。同尺寸改写会返回 `LW_COLLECT_SOURCE_CHANGED`，必须重新预检。
冻结成功后的 Work 修改不改变已保存的对象版本和内容。

## 部署与回退

应用 Control migration `0014_material_upload_identity.sql` 后再启动新版本。它清除未验证文件的路径 hash，增加取消状态和清理调度字段。
不把旧材料包 JSON 当成包含实际内容 hash 的新契约。历史未验证文件要经过新完成流程。已有旧格式材料包需要重传。
对象存储必须启用版本，并支持确切 key 的版本枚举和版本删除。清理权限沿用已有平台镜像清理要求，不扩大前缀授权。
按材料包上限和两个同时验证任务为 Control 临时目录保留空间。临时文件 guard 在成功、失败和取消读取后删除文件。
回退先停止新上传，再协调应用和生成契约。migration 保留新增列和状态，通过前滚修复处理新状态，避免回退时丢失取消或完成事实。
