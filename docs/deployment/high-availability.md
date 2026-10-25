# 三节点持久化与高可用基线

本基线由 Bundle 的 infrastructure inventory 驱动，入口仍是 `cargo xtask platform-foundation`。`platform_foundation_ha` 非空时，现有角色安装 Rook、CloudNativePG（CNPG）和 Barman Cloud 插件，替换独立 PostgreSQL StatefulSet，创建三个 NATS 节点，并把现有 JetStream stream 调整为三副本。没有这个 profile 的本地单节点开发不承诺 HA。

## 版本与选择理由

`deploy/versions.lock.yml` 的 `high_availability` 锁定 chart 版本、归档 SHA-256、镜像 digest 和上游来源。Ansible 先按校验和下载 chart，再安装本地归档；不从浮动 Helm 仓库解析版本。JetStream 复用同一锁中的 `platform_foundation.nats`，不维护第二份镜像身份。

| 组件 | 锁定版本 | 选择依据 |
| --- | --- | --- |
| Rook | 1.19.12 | 1.19 系列支持 Kubernetes 1.30–1.35；采用维护中的 patch，而不是早期候选的 1.19.0 |
| Ceph | Tentacle 20.2.4 | Rook 1.19 支持的稳定系列；Squid 的预计维护截止为 2026-10-31，不能用即将到期的旧候选作为新安装基线 |
| Ceph-CSI | 3.18.1 | 上游测试矩阵覆盖 Kubernetes 1.35；显式覆盖 Rook 的 CSI 镜像及其 sidecar，禁用不使用的 CephFS 和 CSI operator 分支 |
| CNPG | 1.30.1，chart 0.29.1 | 上游维护窗口内，支持 Kubernetes 1.35；不选择已 EOL 的 1.28 来保留旧备份接口 |
| PostgreSQL | 17.6 CNPG standard-trixie | 保持业务 PostgreSQL 主版本，使用 CNPG operand 而非把独立 Alpine 容器直接交给 operator |
| Barman Cloud CNPG-I | 0.15.1，chart 0.8.1 | 使用独立插件归档 WAL；不使用已经退出支持的 in-tree `barmanObjectStore` 路径 |
| NATS / JetStream | 2.14.1 | 复用现有 JWT、双向 TLS 与版本锁；服务端和每条持久 stream 都是三副本 |

上游依据：[Rook 前提](https://rook.io/docs/rook/v1.19/Getting-Started/Prerequisites/prerequisites/)、[Ceph 维护窗口](https://docs.ceph.com/en/latest/releases/)、[Ceph-CSI 矩阵](https://github.com/ceph/ceph-csi/tree/v3.18.1)、[CNPG 支持窗口](https://cloudnative-pg.io/docs/1.30/supported_releases/)、[CNPG 同步复制](https://cloudnative-pg.io/docs/1.30/replication/)、[CNPG failover quorum](https://cloudnative-pg.io/docs/1.30/failover/)、[Barman 插件](https://cloudnative-pg.io/plugin-barman-cloud/docs/installation/)。上游支持声明与固定镜像存在不等于本平台组合通过硬件故障验收。

## 硬件、容量与故障域

1. 使用三个独立物理节点；三个 VM 或 Kind 节点放在同一宿主机不构成三个硬件故障域。三个节点必须能承载故障后剩余的控制面、数据库和消息负载。Kubernetes API/etcd 自身也必须具备三成员 quorum。
2. 每节点至少给基础设施预留 8 vCPU、16 GiB RAM，另算业务预算；每个 OSD 请求 1 CPU、4 GiB，mon 请求 500m/1 GiB，数据库每实例请求 1 CPU/2 GiB。不能把这些 requests 当作整机充足性的证明。
3. 每节点至少一块独立、无文件系统且未挂载的原始 SSD/NVMe；系统盘与 OSD 分离。使用 `/dev/disk/by-id/…` 显式列盘，禁止 `useAllDevices`、自动擦盘和 loop 设备。安装前核对 `lsblk -f`、设备序列号与维护窗口。生产清空另行授权，本角色不删除旧数据或清理主机目录。
4. Linux 内核至少 5.11，并具备 RBD 模块；安装前在各节点检查 `modprobe rbd`。基线启用 msgr2 与传输加密。节点之间至少 10 GbE、低延迟且稳定的双向连通；隔离存储流量与用户突发流量，避免同一交换机故障被误认为三个独立故障域。
5. Ceph 三个 mon 不共节点，两个 mgr 不共节点，pool 的 CRUSH `failureDomain=host`，`size=3`、`min_size=2`。原始容量除以三才是未计开销的可用上限；按最多 70% 规划业务使用量，为 backfill 留空间。OSD nearfull/backfillfull/full 为 80%/85%/90%。不在故障期间降低副本数、`min_size` 或满盘阈值。
6. `ceph-rbd` 只用于 RBD Filesystem、`ReadWriteOnce`、`Retain`；使用 exclusive-lock、object-map、fast-diff，允许扩容。provider 的现有 `workspaceStorageClassName`/`workspaceAccessMode` 必须选择 `ceph-rbd`/`ReadWriteOnce`。不新增平行 storage profile；不声称支持 RWOP、共享可写文件系统或未经验证的 VM Block RWX。
7. RWO、exclusive-lock、Pod 消失或 VolumeAttachment 消失都不单独证明旧节点已停止写盘。失联节点的跨节点重新挂载仍须通过 #207 的 fencing 入口；不能通过强删 Pod、force detach 或强制 break lock 绕过。`Retain` 也不等于业务七天保留或授权删除。

## Bundle 输入

把以下内容放进**现有 Bundle** `infrastructure.inventoryFile` 指向文件的 `all.vars`，不要另建 Helm values 真源。示例节点、域和设备必须替换为实际输入；秘密仍用 Bundle 文件引用。

```yaml
platform_foundation_ha:
  failureDomain: production-site-a
  nodes:
    - name: node-a
      devices: [/dev/disk/by-id/REPLACE-node-a-ssd]
    - name: node-b
      devices: [/dev/disk/by-id/REPLACE-node-b-ssd]
    - name: node-c
      devices: [/dev/disk/by-id/REPLACE-node-c-ssd]
  productionObjectEndpoint: https://objects.example.edu
  postgresStorage: 100Gi
  natsStorage: 20Gi
  backup:
    endpoint: https://independent-backup.example.net
    destination: s3://labweaver-backups/postgres/
    failureDomain: backup-site-b
    credentialsSecret: independent-backup-secrets
    caSecret: independent-backup-secrets
    archiveTimeoutSeconds: 300
    baseBackupSchedule: '0 0 2 * * *'
    retentionDays: 30
    userDataSchedule: '0 0 3 * * *'
    userDataRetentionDays: 30
```

身份库不复用业务库的 Cluster、应用凭据或备份对象前缀。在同一个 inventory 的 `all.vars` 增加：

```yaml
identity_foundation_enabled: true
identity_foundation_ha:
  failureDomain: production-site-a
  productionObjectEndpoint: https://objects.example.edu
  backup:
    endpoint: https://independent-backup.example.net
    destination: s3://labweaver-identity-backups/keycloak/
    failureDomain: backup-site-b
    credentialsSecret: identity-backup-secrets
    caSecret: identity-backup-secrets
    archiveTimeoutSeconds: 300
    baseBackupSchedule: '0 0 2 * * *'
    retentionDays: 30
```

身份备份可使用同一个独立备份站点，但必须使用独立 bucket 或互不包含的对象前缀与受限凭据；不能与业务库共用归档目录。`infrastructure.identitySecretFile` 指向现有 `identity-bootstrap.v2` YAML，保留 `database_password`、`bootstrap_admin_password`、`service_client_secrets`、`revision`，另提供 `backup_access_key`、`backup_secret_key`、`backup_ca_crt`。这些秘密仅在加密 Bundle/受限 locator 内保存，由角色在身份 namespace 创建独立 `identity-backup-secrets`；不要复制业务库 Secret。`labweaver-keycloak-database` 使用 `kubernetes.io/basic-auth`，用户名固定为 `keycloak`。CNPG 自行管理身份库的 CA、服务与复制证书，不复用业务库或 Keycloak HTTP 证书。

生产 HA Bundle 或启用了身份 foundation 的 Bundle 均要求该身份备份配置和凭据；缺少目标、故障域、凭据或非法频率/保留期时，以 `LW_BUNDLE_HA_INVALID:identity_foundation_ha.<字段>` 在执行前拒绝。身份 profile 不接受用户卷备份计划。非 Bundle 全量身份入口也校验同样的目标规则，并要求有效的备份 CA 材料。默认 `identity_postgres_storage=20Gi`，存储类固定 `ceph-rbd`，不再提供身份库单副本/local-path 分支。


生产 HA profile 不接受未配置的独立备份。缺失/非法字段以 `LW_BUNDLE_HA_INVALID:platform_foundation_ha.<字段>` 阻止 Bundle 校验。目的地址必须是 HTTPS，主机名不能与生产对象端点相同，声明的故障域也必须不同。域名不同不能证明物理独立：部署者必须核对备份目标的电源、主机、磁盘、网络与站点；同一生产 RGW 上的另一个 bucket 不合格。

在同一 foundation 私有 render-input 的 `secrets/postgres-secrets/` 放入 `backup-access-key`、`backup-secret-key`、`backup-ca.crt`；用已有 `tools/render_platform_bundle.py` 和 foundation manifest 生成 Bundle 引用的基础设施对象。HA renderer 从这三个受控字段派生 `independent-backup-secrets`；不生成假凭据。缺材料时不会启动没有归档目的地的生产 CNPG。更新 CA、NATS TLS 服务证书时，用现有 `tools/prepare_platform_foundation.py` 重新生成；NATS 路由证书需要 serverAuth/clientAuth 与三个成员的 DNS SAN。

同一 authoring 工具还签发 `streaming_replica` 客户证书；HA renderer 派生 CNPG 要求的 `kubernetes.io/tls` 类型 Secret，并沿用平台现有 CA 与 `postgres-admin` 证书认证。平台 CA 私钥不进入这些 Secret。已有旧 foundation 输入缺少 `replication.crt` / `replication.key` 时须先重新签发，不能让复制或管理员认证悄悄换用另一套 CA。

Bundle 派生应用/Resource Helm 输入时，为 Environment、Resource 和两个执行器注入只读 Ceph API 地址，并开启它们的 Kubernetes token 投影。应用与 Resource namespace 也从 Bundle target 派生，不在 inventory 另写。沿用 `network.kubernetesApiCidrs` / `network.kubernetesApiPorts` 的现有受控 API egress。健康读取 Role 只允许 `get rook-ceph/rook-ceph` 这一 CephCluster，不授予列表、exec 或写集群权限。

HA Bundle 还校验实际 `providers.json`：容器必须选择 `workspaceStorageClassName=ceph-rbd`、`workspaceAccessMode=ReadWriteOnce`；VM 的基线和动态基线存储也必须选 `ceph-rbd`。指向 NFS/local-path 或共享写模式的输入按字段拒绝，不能读取 Ceph 的健康却准入另一套存储。

本基线不部署 Kubernetes VolumeSnapshot/VolumeGroupSnapshot CRD，也不启动对应 CSI snapshotter；RBD 备份/恢复的执行能力由 #208 配套交付，不能从 CSI 镜像包含某个 RPC 推导已经具备备份或恢复能力。

## 持久性、故障与拒绝准入

CNPG 创建三个不同 host 上的实例、真实 operator 就绪检查与 PDB。`synchronous.method=any`、`number=1`、`dataDurability=required`，提交必须等待至少一个同步 standby 的 WAL 落盘；`synchronous_commit=on`，丢失同步成员不降成异步写入。启用 `failoverQuorum`，保留主节点隔离检查和主租约，`smartShutdownTimeout=0` 避免分区时等待旧会话继续提交。HA 客户端直接连接 CNPG 管理的 `postgres-rw.labweaver-data.svc`；Bundle 从同一个 HA profile 派生业务/Resource 的数据库 URL、控制器 port-forward、EndpointSlice 查询与 TLS server name，保留原凭据及 TLS 参数。管理 SQL 读取当前 primary，不固定 `postgres-0`；不创建无 selector 的 `postgres` 别名。

冻结 worker 的入站规则单独选择 CNPG Pod，只允许 TCP 5432。VM worker namespace 来自 Bundle 中实际 Evaluation coordinator 的 `vmJobNamespace`；容器 worker 位于受管环境 namespace，采用现有环境标签；已有落点从同一配置的 placement profile 派生。两类来源都必须同时具备真实 Job 使用的 `app.kubernetes.io/name=evaluation-freeze-worker` 和 `app.kubernetes.io/managed-by=labweaver-freeze-coordinator` Pod 标签，不能把整个环境 namespace 或全部租户 Pod 放行。

NATS StatefulSet 是三个成员、host 必须反亲和、PDB 至少保留两个。路由通过既有 CA 的双向 TLS 连接；JWT 用户权限不扩大。stream 创建和 readback 都要求 `num_replicas=3`，已有 stream 通过原有配置角色调整。失去 quorum 后 JetStream 自身不确认不能提交的写入；消费者跟随 stream 的复制策略。禁止通过重建单副本 stream 隐藏丢失的 quorum。

Resource 在批准、Task claim、新 capacity shell 前读取 CephCluster；Environment 在解析运行授权（包括正向执行侧检查）时再读一次。每次实际 GET，不缓存最后成功值。只有 `phase=Ready`、`HEALTH_OK`、`lastChecked` 不超过 30 秒且不是未来时间的观测允许新准入。Rook 健康采样设为 10 秒。nearfull、mon quorum 异常、PG 不可用、API 无法读取、权限失效、超时或过期观测都 fail-closed，分别返回 `LW_STORAGE_UNSAFE`、`LW_STORAGE_HEALTH_UNAVAILABLE`、`LW_STORAGE_HEALTH_STALE`，带中文原因和可重试语义。Resource 的存储拒绝不会阻止容量清理循环运行。

这个健康门不替代 Ceph 写入层安全：现有 I/O 的 quorum/满盘拒写由 Ceph 的 pool 最小副本与 full 阈值执行。Kubernetes 状态不是线性一致的物理存储探测，观测到分区存在至多一个采样/新鲜度窗口；不能由一次 `HEALTH_OK` 宣称未来写入绝对成功。已有授权也不能绕过存储的实际拒写。

**身份链的 HA 边界：** `identity_foundation` 创建独立 `identity-postgres` CNPG Cluster（三实例、host 必须反亲和、required synchronous/quorum、PDB）及独立 ObjectStore/ScheduledBackup。Keycloak 至少两个副本并跨 host 调度；角色先等待身份 Cluster Ready，再启动 Keycloak。每次 Pod 启动的 init container 通过应用口令和 `verify-full` TLS 执行实际 `SELECT 1`；Keycloak JDBC 同样校验证书并连接 CNPG 管理的 `identity-postgres-rw`，不再访问旧 `identity-postgres` Service。CNPG operator 及 Barman 插件必须先由 platform foundation 安装；身份 namespace 的网络策略允许复制、operator、API、DNS 和指定独立备份 HTTPS 出口。此配置不等于真实节点故障、登录全链或站点恢复验收已经完成；#205 仍保持 Open。

## 站点灾难目标与独立备份

以下目标已决定，**兑现待 #208 恢复验收**；不是已测 RPO/RTO：

| 数据 | RPO 目标 | RTO 目标 | 配置/恢复前提 |
| --- | --- | --- | --- |
| PostgreSQL 业务事实（CNPG 全部库） | ≤1 小时 | ≤4 小时 | 连续 WAL 归档，`archive_timeout≤300s`，每日 base backup，独立站点对象目标及可用凭据/CA |
| Work 持久卷、对象存储中的提交/材料 | ≤24 小时 | 平台服务≤4 小时；用户数据按卷≤24 小时 | 每日独立备份；保留期、时间来自 Bundle；#208 接管执行和逐卷恢复 |
| NATS JetStream | 不单列业务 RPO | 随平台恢复 | PG 业务事实与 Outbox 重建；不能把 broker 中尚未落库的任意消息当作可恢复事实 |

WAL、base backup 的 ObjectStore、插件和 ScheduledBackup 已渲染到独立目标；这不是备份执行或恢复演练。`baseBackupSchedule`/`userDataSchedule` 为每日六字段 cron，默认分别 02:00/03:00（使用集群调度时区）；两类保留期默认 30 天、可配置。用户数据的计划与保留期是 #208 的 Bundle 输入，不在此角色假建一个空备份任务。备份容量须按数据库 base 大小、WAL 生成速率、用户数据日增量和保留天数测算，并监测归档失败、容量与可恢复时间点。备份失败不自动降低同步复制或删除生产数据。

两类保留天数必须是正整数且至少 7 天，不满足时在 Bundle 校验阶段以对应字段名 fail-closed。默认数据库备份容量至少按 **30 天 × 每日 base 大小 + 30 天 WAL 增量**估算，再留失败重试与增长余量；用户数据另按全量/增量策略核算。管理员调小保留期会缩短可恢复时间窗口，不能只按节省容量理解该操作。

## 安装与验证边界

1. 在授权窗口核对三个节点、磁盘清单、内核与独立备份。先检查现有 Bundle 的基础设施输入，不手动向集群应用第二份 YAML。
2. 更新 Bundle 里的版本锁和 inventory，重新 seal；沿用 [加密 Bundle 手册](encrypted-bundle.md) 的 `validate`/`exec`、`platform-foundation` 和 `platform-application` 入口。
3. 已有独立 `postgres` StatefulSet 会触发 `PLATFORM_FOUNDATION_HA_CUTOVER_REQUIRED_NO_AUTOMATIC_DATA_DELETION`。在获准的数据切换窗口处理它及旧 `postgres` Service；角色不自动删库、搬数据或清空 PV。HA 只消费实际 `postgres-rw` Service，不保留 ExternalName 或旧名称兼容路径。先完成同一 Bundle 的配置切换，再执行应用初始化/迁移。

   旧 NATS StatefulSet 的 `podManagementPolicy=OrderedReady` 也不能原地修改为 Parallel，会明确触发 `PLATFORM_FOUNDATION_HA_NATS_CUTOVER_REQUIRED_RETAIN_PVCS`。在同一获准窗口完成 StatefulSet 切换并保留原 PVC；角色不自动删除 stream 或卷。单节点开发保持原 OrderedReady，不受 HA 调度策略变更影响。

   身份入口同样拒绝已有 `StatefulSet/identity-postgres` 或旧 `Service/identity-postgres`，返回 `IDENTITY_CNPG_CUTOVER_REQUIRED_NO_AUTOMATIC_DATA_DELETION`。仅在另行授权的**空数据切换窗口**执行[身份库切换步骤](runbook.md#身份库空数据切换仅限另行授权窗口)：停止 Keycloak 写入、保全对象及旧 PVC、删除旧 StatefulSet/Service、重建凭据 Secret，再初始化新 CNPG；按该节验证或回退。`initdb` 创建新库，不迁移旧账号/realm；保留 PVC 不会自动导入身份数据。角色本身不删除旧对象或复制 PGDATA。先执行 platform foundation（CNPG/Barman/Ceph），再执行 identity foundation，最后部署依赖身份的应用。
4. 部署后核对三个 Ceph mon、pool size/min_size/host、CNPG 三实例与同步/quorum、NATS 三成员及每条 stream 副本。读探针或 manifests 渲染不能代替写入后故障恢复。
5. 在独立 `labweaver-ha-*` Kind 中可以检查 CNPG/JetStream 调度与协议，不能以同一宿主的三容器证明物理三节点容灾。不要操作 `labweaver-local-*`。Ceph 需要真实块设备，本机没有为此授权的裸盘时不安装 Rook、不伪造 OSD。
6. 在后续授权窗口记录实际确认写入、单节点故障、quorum 丢失、分区、接近满/满盘、旧节点 fencing、独立备份恢复、实际 RPO/RTO。未完成前 #205 的硬件故障和恢复验收保持未完成。

组件冒烟入口（只创建并清理自己命名的集群，使用私有 kubeconfig）：

```sh
python tools/smoke_ha_foundation.py --name labweaver-ha-smoke --charts .tmp
```

`.tmp` 中需先下载版本锁指定的 CNPG chart 归档；脚本校验归档 SHA-256。组件冒烟为 CNPG/JetStream 使用 Kind 的 `standard` 本地测试卷，不创建假的 Ceph/RBD、不连接生产备份；它不替代生产 profile 的原样渲染、硬件故障或恢复验收。

身份组件可单独运行 `python tools/smoke_ha_foundation.py --name labweaver-ha-identity-smoke --charts .tmp --identity-only`。该入口渲染实际身份 Cluster/Keycloak 模板，临时仅把存储换成 Kind `standard` 并去掉外部备份插件；它验证三 host 上的三个 Ready 实例、同步确认写、两个 Keycloak Ready 副本及真实 realm 初始化/认证 TLS 连接，结束删除自己创建的集群。不安装 Cilium/Ceph，不执行任何备份或恢复演练，不证明生产网络策略或 RPO/RTO。

## RGW 兼容结论与切换条件

**结论：当前不能批准替换生产 MinIO。** Ceph 20.2.4 的 RGW 提供 S3 versioning、Object Lock 和 multipart，但接口存在不证明当前客户端语义通过。此切片未创建生产 RGW、未迁移 bucket，也没有把保留 MinIO 的现状当作 RGW 已兼容。只允许在后续兼容验收/切换窗口短期保留现有路径；不得长期维护两套等价生产对象存储。

需要用 `artifact-store` 的真实 AWS S3 客户端对该固定版本逐项运行：

- bucket versioning 和 Object Lock 在创建时开启；每次 PUT/GET/HEAD 的非空 version ID、指定旧版本读取、精确 SHA-256/长度校验；
- Compliance retention 的 retain-until、未到期的版本删除失败、delete marker 与指定版本删除的差别、到期后受权删除；不能以 governance bypass 取代业务保留；
- 材料条件 PUT 的 `If-None-Match:*` 与 412 重试语义；签名头、Content-Length、校验和及经网关预签名 URL；
- 5,000,000,000 字节平台镜像 multipart、64 MiB 分片的列举/重传/完成/中止，失败分片和不完整上传清理；
- Harbor 固定版本的 registry S3 driver 上传、拉取、GC、TLS、签名与版本/保留配置组合，不能由应用上传测试外推 Harbor 兼容。

RGW 测试通过后，在同一个受控 Bundle 中切换唯一对象端点，再退役旧 MinIO/NFS 等价路径；与 #207 的单写 fencing 和保留/导出规则联动。备份目标仍必须在另一个故障域，不能随着生产端点切换到同一 RGW。

## 验收工具的创作队列观测

`tools/user_acceptance.py` 从实际部署的数据库 Service selector 选择唯一 Ready Pod：HA 使用 CNPG 的 `postgres-rw` 当前 primary，单节点开发仍使用原 `postgres` Service 对应的 Pod。每次观测都重新解析，不缓存主节点名称，primary 切换后不会继续 exec 已失效的固定 Pod。

无法读取 Service/primary、没有唯一 Ready primary、SQL 查询失败或计数无效时，队列状态为未知。`preflight` 以 `LW_ACCEPTANCE_AUTHORING_QUEUE_UNAVAILABLE` 明确失败，不把未知当作空队列或成功；默认等待队列的 `run` 同样停止。`--no-queue-wait` 仍表示操作者明确跳过等待。
