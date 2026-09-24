# LabWeaver v1 部署运行手册

本手册是 Issue #127 简化部署路径的操作入口，面向 `deploy/ansible/inventories/v1/` 这套真实集群（维护者控制器为本工作站，SSH 用户 `labweaver-deploy`）。所有 `cargo xtask` 子命令与 playbook 均以当前树为准；本轮不再包含 TestFlight 报告、InfrastructureDeploymentManifest/HarborPolicyManifest/BackupEvidence 哈希链、foundation clean-redeploy/finalize、rotation-verify、NATS authority rotation、`xtask backup`/`xtask rollback` 或 credential-registry。

> 警告：本轮没有备份与回滚工具。删除数据卷、数据库或共享基础组件后无法从本仓恢复。执行前必须确认变更窗口、受影响用户和数据保留需求。应用层 Helm 升级使用 `--atomic`，失败时只回退到上一 Helm revision，不等于数据库回滚。

## 1. 前提

### 1.1 控制器与仓库

控制器是本工作站，仓库根目录为 `LabWeaver`。先安装锁定工具（版本见 `deploy/ansible/controller.lock.yml` 与 `deploy/versions.lock.yml`）：

```sh
cargo --version                 # rust_toolchain 见 deploy/versions.lock.yml
docker-buildx version           # buildx v0.35.0
helm version --short            # v3.21.3
kubectl version --client
ansible --version               # 仅手工 ansible-playbook 时需要 ansible-core 2.18.6
virtctl version                 # 验证阶段在控制平面 PATH 上必需
```

按需安装 Ansible collections（xtask 内置 ansible-rs 也读取同一 collections/roles 路径）：

```sh
ansible-galaxy collection install -p deploy/ansible/collections -r deploy/ansible/requirements.yml
```

`xtask` 通过 `set_system_envs` 继承当前 shell 环境，因此下面导出的变量会传入 playbook。若控制器把 collections 放在仓库之外，设置 `LABWEAVER_ANSIBLE_DEPENDENCY_ROOT` 指向包含 `collections/`（或 `deploy/ansible/`）的目录。

### 1.2 私有清单、SSH 与 kubeconfig

真实清单与 vault 口令在仓库中被 gitignore，必须存在：

```sh
ls -l deploy/ansible/inventories/v1/hosts.yml deploy/ansible/inventories/v1/.vault-password
```

- `hosts.yml`：节点、`ansible_user`、私钥路径、`group_vars` 变量。模板见 `deploy/ansible/inventories/v1/hosts.yml.example`。
- `.vault-password`：xtask 以 `ANSIBLE_VAULT_PASSWORD_FILE` 注入。
- 集群管理员 kubeconfig：本工作站常用 `.private/kubeconfig-v1-admin.conf`；xtask 的 connected 校验与产品部署读取 `LABWEAVER_KUBECONFIG`。角色默认在目标机使用 `/etc/kubernetes/admin.conf`。

```sh
export LABWEAVER_RUN_ID="infra-redeploy-1"        # 小写 8-96 字符或 UUIDv7；identity 需 ^infra-
export LABWEAVER_PLATFORM_REGISTRY="harbor.lab.lan" # 裸主机名，不带 scheme/路径
export LABWEAVER_KUBECONFIG="$PWD/.private/kubeconfig-v1-admin.conf"
```

`LABWEAVER_RUN_ID` 是 xtask 的强制运行标识，用于隔离验证命名空间与部署工作目录。

### 1.3 Harbor 凭据与公共入口

- 内部 Registry：`harbor.lab.lan`，项目 `labweaver-system`（`platform_harbor_route` 强制 `harbor_project_name == 'labweaver-system'`，项目为 private、auto_scan、prevent_vul=false）。
- 机器人凭据文件：`.private/harbor-robot-user`、`.private/harbor-robot-pass`；Harbor 公共 CA 由 `platform_harbor_route` 发布到 `/var/lib/labweaver/.private/harbor-public/registry-ca.crt`。
- 公共入口由 `deploy/ansible/roles/public_ingress` 声明（当前树默认值）：
  - `https://labweaver.2018wzh.top`、`https://portal.labweaver.2018wzh.top` → `labweaver-system/web:8080`
  - `https://keycloak.labweaver.2018wzh.top` → `keycloak-system/labweaver-keycloak-http:8080`
  - `https://harbor.labweaver.2018wzh.top` → `harbor/harbor:80`
- 内部门户路由在私有 values 中可覆盖（例如当前 `.private/helm-values-current.yaml` 使用 `portal.labweaver.internal`）。以私有清单和实时 HTTPRoute 为准。

### 1.4 节点事实（preflight 强制项）

`cargo xtask preflight` 运行 `00-preflight.yml`，强制：

- 清单分组：`routers` 恰好 1、`control_plane` 恰好 1、`workers` ≥ 2、`nfs_servers` 恰好 1。
- 无未解析的 `REPLACE_`/空清单变量。
- OS 为 Rocky/RHEL 10+ 或 Ubuntu 22.04+；RedHat 需 SELinux `Enforcing`。
- worker 必须有字符设备 `/dev/kvm` 且 CPU 暴露 `vmx`/`svm`。
- NFS 服务 active、导出存在、集群节点可达 2049。
- 路由器 WAN/LAN 接口存在（未设置 `labweaver_skip_router_services` 时）。

preflight **不检查 GPU**。启用 GPU 前需人工确认：NVIDIA 驱动、`nvidia-container-toolkit`、设备插件，以及 KubeVirt mediated device 所需的内核与节点配置（见第 6 节）。

### 1.5 运行预检

```sh
cargo xtask preflight --env v1 --infra
```

手工等价命令（在 `deploy/ansible` 下执行，`ansible.cfg` 生效）：

```sh
ansible-playbook -i inventories/v1/hosts.yml playbooks/00-preflight.yml \
  --vault-password-file inventories/v1/.vault-password \
  -e labweaver_preflight_scope=cluster
```

## 2. 构建与发布镜像

打包在干净的源码树上进行（脏树会被 `LW_PACKAGE_INPUT_DIRTY` 拒绝），并锁 Rust 工具链与摘要固定的基础镜像。

```sh
export LABWEAVER_PLATFORM_REGISTRY=harbor.lab.lan
cargo xtask package --env v1 --release platform-1 --profile platform --yes
cargo xtask package --env v1 --release resource-1 --profile resource --yes
```

- `platform` profile 构建除 `resource-service` 外的组件；`resource` profile 只构建 `resource-service`。
- 输出：`artifacts/package/pkg-v1-<release>-<commit12>/PlatformImagePackageManifest.json`（JCS 规范化，记录 commit、组件锁哈希、builder 版本、digest 引用）。
- 基础镜像镜像化预期：每个基础镜像必须已按摘要镜像到 `<registry>/labweaver-system/base-<name>@sha256:<digest>`（如 `base-rust-builder`、`base-web-runtime`）。构建参数只使用这些私有镜像；缺失或可变 tag 会失败。
- BuildKit 身份：本地 `docker-buildx inspect --bootstrap` 必须匹配锁定的 `platform_images.buildkit`；若本地不是该 BuildKit 镜像，则通过 `LABWEAVER_KUBECONFIG` 读取 `labweaver-build/buildkit` Deployment 校验镜像、配置注解与就绪副本。

校验：

```sh
cargo xtask package-validate \
  --manifest artifacts/package/<run-id>/PlatformImagePackageManifest.json \
  --mode static

cargo xtask package-validate \
  --manifest artifacts/package/<run-id>/PlatformImagePackageManifest.json \
  --mode connected --env v1
```

`connected` 会重新校验组件锁哈希、工具身份，并逐个 `docker-buildx imagetools inspect` 确认 Harbor 当前摘要与清单一致。

### 2.9 迁移目录与在线 ledger 的前缀校验（部署前必做）

`migrations/catalog.yaml` 是迁移的唯一真源，而每个域在数据库里都有 `schema_migrations`
（`migration_id` + `sha256` + `outcome`）。发布前必须确认**在线 ledger 是目录的有序前缀**，否则
服务启动会以 `DB_SCHEMA_CHECKSUM_MISMATCH`/`DB_SCHEMA_UNKNOWN` 失败关闭：

```sh
kubectl -n labweaver-data exec postgres-0 -- env PGPASSWORD="$(kubectl -n labweaver-data get secret postgres-secrets -o jsonpath='{.data.postgres-password}' | base64 -d)" \
  psql -U postgres -d labweaver -tAF'|' -c "select 'control',migration_id,sha256,outcome from control.schema_migrations union all select 'access',migration_id,sha256,outcome from access.schema_migrations union all select 'environment',migration_id,sha256,outcome from environment.schema_migrations union all select 'agent',migration_id,sha256,outcome from agent.schema_migrations union all select 'evaluation',migration_id,sha256,outcome from evaluation.schema_migrations union all select 'resource',migration_id,sha256,outcome from resource.schema_migrations order by 1,2"
```

逐域比对（id 与 `sha256` 都要相等），新增迁移只能追加在末尾。`#127` 的 rebase 正是按这条规则把
develop 的新迁移顺延编号，保持在线 ledger 不变。

## 3. 平台层干净重部署

当前树**没有** foundation clean-redeploy 专用 playbook（`94-foundation-clean-redeploy.yml`、`platform_reset`、`foundation_clean_redeploy*` 已删除）。以下步骤用明确的 `helm`/`kubectl`/`ansible-playbook` 手工完成；每一步先记录、再删除，且删除范围必须精确。

### 3.1 冻结并记录现状

```sh
export KUBECONFIG="$LABWEAVER_KUBECONFIG"
helm -n labweaver-system list
helm -n labweaver-data list
kubectl get ns
kubectl get pv    # 记录 Released 且 claimRef 指向应用命名空间的 PV
kubectl get vmi,vm -A
kubectl get ns -l labweaver.io/environment=true
```

停止用户环境、VM 与评测 Job 后再继续。停止请求不代表资源已释放或免费；未确认释放的用量保持待核实。

### 3.2 删除应用层（精确范围）

应用层 Helm release 与命名空间：

```sh
helm -n labweaver-system uninstall labweaver-resource || true
helm -n labweaver-system uninstall labweaver || true
kubectl delete namespace labweaver-system labweaver-evaluation --ignore-not-found --wait=true
```

> 删除 `labweaver-system` 会连带删除该命名空间内的 PVC/DataVolume（包括平台 VM base `ubuntu-lab-base-v1-seed`，重部署时会从 CDI 重新导入）。`labweaver-data`（PostgreSQL/NATS/MinIO）与 `keycloak-system` 属于基础层，**不受**上述命令影响。

只有确认要重建基础层时，才在单独确认后删除其 PVC/PV（`labweaver-data` 的 `data-postgres-0`/`data-nats-0`/`data-minio-0`、`labweaver-build` 的 BuildKit 卷）。这一步不可恢复。

删除已释放的应用层 PV（逐个精确名称，不要用 label/通配批量删除）：

```sh
kubectl get pv -o custom-columns=NAME:.metadata.name,CLAIM_NS:.spec.claimRef.namespace,STATUS:.status.phase
kubectl delete pv <exact-pv-name> --wait=true
```

### 3.3 清理残留

应用残留清理见第 8 节。节点级旧集群残留属于另一条路径（`15b-v1-residue-cleanup.yml`，需 `-e labweaver_v1_cleanup_old_cluster=true`），不是应用残留清理工具。

### 3.4 重装基础层

按顺序执行（幂等 reconcile，均需 `--infra --yes`）：

```sh
cargo xtask platform-foundation --env v1 --infra --yes   # labweaver-data: postgres/nats/minio
cargo xtask platform-buildkit   --env v1 --infra --yes   # labweaver-build: rootless BuildKit
cargo xtask platform-harbor-route --env v1 --infra --yes # 采用既有 Harbor，仅路由与信任
```

若 Harbor 本身需要安装或重建（会先用 `95-harbor.yml`）：

```sh
cargo xtask deploy --env v1 --infra --yes
```

若身份基础层（Keycloak）需要重装或校验：

```sh
export LABWEAVER_IDENTITY_SECRET_LOCATOR="<root-only identity bootstrap locator>"
cargo xtask identity-foundation --env v1 --infra --yes --action deploy
cargo xtask identity-foundation --env v1 --infra --yes --action verify
```

`identity-foundation` 要求 `LABWEAVER_RUN_ID` 匹配 `^infra-`，且控制器持有 root-only、`0600`、SHA-256 与清单 `identity_secret_locator_sha256` 一致的 bootstrap 文件。

### 3.5 应用配置包

平台配置包由公开清单约束的私有输入渲染，渲染脚本拒绝覆盖已有输出，且只在 `.private` 目录内读写：

```sh
python tools/render_platform_bundle.py \
  --input /var/lib/labweaver/.private/v1/platform-application/render-input \
  --output /var/lib/labweaver/.private/v1/platform-application/configuration-bundle.yml

python tools/render_resource_bundle.py \
  --input <resource-private-render-input> \
  --output /var/lib/labweaver/.private/v1/resource-application/configuration-bundle.yml
```

角色读取的实际 locator 由环境变量与 `application-vars.yml` 提供（`platform_application` 的 env 默认项均可在树中核对）。部署前确认私有目录内已存在：`configuration-bundle.yml`、`application-vars.yml`、`access-seed.json`、Keycloak 用户口令、Anthropic token 及渲染输入。

### 3.6 部署 platform profile

```sh
export LABWEAVER_APPLICATION_VARS_FILE=/var/lib/labweaver/.private/v1/platform-application/application-vars.yml
export LABWEAVER_POSTGRES_SERVICE=platform-admin
export LABWEAVER_POSTGRES_SERVICE_FILE=<platform-admin.service>
export LABWEAVER_POSTGRES_CLIENT_CERTIFICATE_FILE=<postgres client cert>
export LABWEAVER_POSTGRES_CLIENT_PRIVATE_KEY_FILE=<postgres client key>

cargo xtask platform-application --env v1 --infra --yes \
  --package-manifest artifacts/package/<platform-run-id>/PlatformImagePackageManifest.json
```

playbook `93-platform-application.yml` 在 `localhost`（控制器）执行，读取上述 locator、`application-vars.yml` 指向的配置包、Harbor/Keycloak/MinIO/NATS 端点，执行迁移基线、Access seed、JetStream stream/consumer、bucket versioning、Keycloak realm reconcile，最后以 `helm upgrade --install labweaver ... --atomic --wait` 部署并二次应用同一 profile。

### 3.7 部署 resource profile

```sh
export LABWEAVER_RESOURCE_CONFIGURATION_BUNDLE=/var/lib/labweaver/.private/v1/resource-application/configuration-bundle.yml
export LABWEAVER_ACCESS_SEED_FILE=/var/lib/labweaver/.private/v1/platform-application/access-seed.json
export LABWEAVER_RESOURCE_VALUES_FILE=<resource helm values>
export LABWEAVER_POSTGRES_SERVICE_FILE=<platform-admin.service>

cargo xtask resource-application --env v1 --infra --yes \
  --package-manifest artifacts/package/<resource-run-id>/PlatformImagePackageManifest.json
```

playbook `94-resource-application.yml` 只启用 `resource-service`，显式关闭其他 workload，并应用 `resource-service-config`/`resource-service-secrets`。

v1 实测补充（三项都会让 resource profile 失败关闭）：

- `LABWEAVER_POSTGRES_SERVICE_FILE` 必须指向**控制器端口转发**的 service 文件（`hostaddr=127.0.0.1`、
  `port=15432`、`host=postgres.labweaver-data.svc`）。私有源文件里的 `hostaddr` 是集群内地址，控制器不可路由，
  直接用会以 `RESOURCE_APPLICATION_ACCESS_SEED_APPLY_FAILED` 失败。
- Access seed 是**精确**的：它会校验课程成员数。issuer 改为公网名后，旧 issuer 生成的那批 actor
  成员仍在库里，必须按 actor id 精确删除，否则以
  `LW_RESOURCE_ACCEPTANCE_PROFILE_ACCESS_MEMBERSHIP_CONFLICT` 失败。
- `resource-service-config/capacity.json` 必须带 `gpuCatalogSeed`，且与 `deploy/versions.lock.yml` 的
  `platform_gpu_classes` 逐字段一致（`class`/`mode`/`providerBinding`/`capacityUnits`/`allocationBinding`），
  否则以 `PLATFORM_APPLICATION_GPU_CLASS_CATALOG_MISMATCH` 失败。目录项只创建一次、不回写；没有对应
  `gpuObservers` 时该项保持不可用（失败关闭，不得降级为 Mock 或普通容器）。
- `resource-service-secrets` 必须与 bundle 的 `data` **逐键相同**（模块只应用 ConfigMap，Secret 视为
  operator 拥有的不可变材料），否则以 `RESOURCE_APPLICATION_SECRET_OWNERSHIP_CONFLICT` 失败。
  `platform_application` 会统一平台 mTLS 信任根，因此 issuer/信任根迁移后该 Secret 的 `mtls-ca.pem`
  需要由操作者按 bundle 显式接管（`kubectl apply --server-side --force-conflicts`），其余键必须保持一致。

## 4. 简化验证

```sh
cargo xtask verify --env v1 --infra --yes
```

`verify` 运行 `90-verify.yml` 的真实探针，并在 `always` 中清理临时资源：

- 精确节点集合与 `verify_expected_kubernetes_nodes` 比较；所有节点 `Ready`。
- 在隔离命名空间 `labweaver-verify-<run_id>` 创建 RWO（local-path）与 RWX（nfs-rwx）PVC 及 writer/reader Pod，分别读回 `rwo-ok`、`rwx-ok`。
- 在 `labweaver-demo` 创建探针 backend + HTTPRoute，经 `gateway_vip` 以 `verify_gateway_hostname` 主机头读回 `gateway-ok`。
- KubeVirt：`virtctl start kvm-probe`，等待 VMI `Running`，`virtctl version`，`virtctl console` 连接（rc 0 或 124），停止并确认 VMI 删除。
- Cilium DaemonSet `desiredNumberScheduled == numberReady`。
- 无论成败都删除 `app.kubernetes.io/part-of=labweaver-verify` 带 `labweaver.io/verify-run` 的精确资源与隔离命名空间；清理失败报 `VERIFY_CLEANUP_FAILED`。

该阶段不重新校验 Harness 供应链证明，也不要求固定镜像数量。

## 5. GPU 启用

当前树**没有** GPU Ansible 角色或 playbook；设备插件与 KubeVirt mdev 由集群运维在仓外配置。Resource 侧只从 GPU 目录与只读容量观测解析，不接受调用者篡改模式。

### 5.1 模式与资源名（互不混淆）

- `exclusive`：整卡独占；目录 `allocationBinding` 绑定设备插件暴露的独占扩展资源名（例如 `nvidia.com/gpu`）。
- `container_time_slice`：容器时间片，每个工作负载一个共享份额，**不代表独占显存或比例算力**；必须绑定与独占项不同的扩展资源名（例如设备插件时间片配置暴露的 `nvidia.com/gpu.shared`），且 `count` 固定为 1（契约强制）。
- `vm_vgpu`：KubeVirt mediated device（mdev）规格；`allocationBinding` 对应已配置的 KubeVirt 设备规格，依赖节点 mdev 与驱动。

### 5.2 运维步骤

```sh
# 1) 确认设备插件/驱动实际暴露的扩展资源名
kubectl get nodes -o json | grep -o '"nvidia.com/gpu[^"]*"' | sort -u
kubectl -n kube-system get daemonset | grep -i nvidia

# 2) vGPU：确认 KubeVirt mediated devices 配置与节点 mdev
kubectl -n kubevirt get kv kubevirt -o jsonpath='{.spec.configuration.mediatedDevicesConfiguration}'
ls /sys/bus/pci/devices/*/mdev_supported_types 2>/dev/null
```

独占与时间片必须使用不同扩展资源名，仅给工作负载加模式标签不能区分真实调度。

### 5.3 登记 GPU 目录

Resource 管理接口 `POST /api/v1/resource/gpu-catalog`（需管理员 principal 与 `Idempotency-Key`）写入 `GpuCatalogEntry`：

```json
{
  "id": "<uuidv7>",
  "class": "gpu-exclusive",
  "mode": "exclusive",
  "providerBinding": "<resource provider binding>",
  "capacityUnits": 1,
  "allocationBinding": "nvidia.com/gpu",
  "revision": 1,
  "active": true
}
```

时间片项用 `"mode": "container_time_slice"` 且 `allocationBinding` 取另一个扩展资源名；vGPU 项用 `"mode": "vm_vgpu"`。同一 Provider 的独占与时间片绑定同名会在创建时被拒绝（`InvalidGpuCatalog`）。

### 5.4 容量观测

`resource-service-config/capacity.json` 需要为每个 `providerBinding` 配置只读观测，否则该 provider 的 GPU 目录项保持不可用：

```json
{
  "pollIntervalMilliseconds": 1000,
  "environmentHandoff": {
    "baseUri": "https://environment-service:9446/",
    "caFile": "/etc/labweaver/secrets/mtls-ca.pem",
    "timeoutMilliseconds": 5000,
    "systemActorId": "00000000-0000-7000-8000-000000000001"
  },
  "gpuObservers": [
    {
      "providerBinding": "<resource provider binding>",
      "apiServer": "https://<kube-apiserver>:6443",
      "bearerTokenFile": "/etc/labweaver/secrets/gpu-observer-token",
      "clusterCaFile": "/etc/labweaver/secrets/cluster-ca.crt",
      "requestTimeoutMilliseconds": 5000,
      "observationTtlSeconds": 60,
      "maxNodes": 100,
      "maxPods": 10000
    }
  ]
}
```

### 5.5 失败必须关闭

设备缺失、观测缺失/过期、数据不完整或释放未确认时，分配必须明确失败并保留原始诊断；不得回退到 Mock、普通容器或其他 GPU 模式，也不得把未知容量当作零继续准入。

## 6. 沙箱运行时（gVisor on containerd）

平台的一次性负载（Agent authoring attempt、OJ run、Ansible probe）都以
`runtimeClassName: labweaver-sandbox` 运行，隔离边界是 gVisor。`#127` 在 v1 集群上实测得到两条
结论，二者都决定了本节的部署形态：

- **CRI-O 不能提供按 Pod 的沙箱运行时**：CRI-O 1.35 会用节点默认运行时创建 Pod 的 sandbox
  （pause）容器，并在日志里记为 `Ran pod sandbox … with infra container`，但该容器没有任何
  runtime 进程、conmon 或 state 文件；于是 gVisor 的应用容器全部以
  `cannot load sandbox: open /run/…/<id>_sandbox:<id>.state: no such file or directory` 失败。
  （复核方式：把默认运行时换成"拒绝 create"的包装脚本，sandbox 仍然"成功"。）
- **gVisor 读取 CRI 标准注解**：`runsc` 依据 `io.kubernetes.cri.container-type`、
  `io.kubernetes.cri.sandbox-id` 等注解区分 sandbox 容器与普通容器，而 CRI-O 只写
  `io.kubernetes.cri-o.*`；containerd 写标准名，所以换到 containerd 后无需任何注解翻译层。

因此 sandbox 能力的节点运行 **containerd**（控制平面节点保留原 CRI）。`deploy/ansible/roles/sandbox_runtime`
是唯一入口，playbook `75-sandbox-runtime.yml` 在 `site.yml` 中于 KubeVirt 之后、addons 之前对
`workers` 执行，并从控制平面发布 `RuntimeClass`。角色做四件事：

1. 安装锁定版 containerd 与 runc（`deploy/versions.lock.yml` 的 `containerd.*`）到 `/usr/local/bin`，
   以及 `containerd.service` 与 `/etc/systemd/system/containerd.service.d/99-proxy.conf`（校园网代理）。
2. 安装锁定版 gVisor 整树到 `/usr/local/bin`（`runsc`、`containerd-shim-runsc-v1` 与同级
   `gvisor-bin/` sentry；`runsc` 依赖同级 sentry 树，containerd 按名字在 `PATH` 上找 shim）。
3. 写 `/etc/containerd/config.toml`：默认运行时 `runc`（`SystemdCgroup = true`）、
   `labweaver-sandbox` → `io.containerd.runsc.v1`、每个运行时 `sandboxer = podsandbox`、
   pin 的 Pod sandbox 镜像、GPU 节点保留 `nvidia` 设备运行时、CNI 目录
   `/etc/cni/net.d` + `/opt/cni/bin`，并把 `image_pull_progress_timeout` 提到 30m
   （校园网代理下默认 5 分钟会中断大镜像）。
4. 把 kubelet 的 `containerRuntimeEndpoint` 指向 `unix:///run/containerd/containerd.sock`、
   保留评审过的 `podPidsLimit`、停用并禁用旧 CRI（`crio`）、删除其沙箱 drop-in，然后逐项回读。

施加（需 root，私有 inventory 见 §1.2）：

```sh
cd /home/wzh/LabWeaver/deploy/ansible
sudo env ANSIBLE_COLLECTIONS_PATH=/home/wzh/LabWeaver/deploy/ansible/collections \
  ansible-playbook -i /var/lib/labweaver/v1-controller/deploy/ansible/inventories/v1/hosts.yml \
  playbooks/75-sandbox-runtime.yml
```

切换运行时会让节点上所有容器重建，应先 `kubectl drain`，完成后再 `uncordon`。重启节点后
`containerd`/`kubelet` 必须自动 active、`crio` inactive（v1 两台 worker 已实测）。

逐节点回读：

```sh
containerd --version
runc --version
/usr/local/bin/runsc --version
systemctl is-active containerd kubelet crio
containerd config dump | grep -A3 "runtimes.labweaver-sandbox"
grep -n containerRuntimeEndpoint /var/lib/kubelet/config.yaml
kubectl get runtimeclass labweaver-sandbox -o jsonpath='{.handler}'
```

最小沙箱探针（一次运行、用完即删；安全上下文必须满足命名空间的 restricted 策略）：

```sh
kubectl -n labweaver-evaluation apply -f - <<'YAML'
apiVersion: batch/v1
kind: Job
metadata: {name: sandbox-probe, namespace: labweaver-evaluation}
spec:
  backoffLimit: 0
  template:
    spec:
      runtimeClassName: labweaver-sandbox
      restartPolicy: Never
      automountServiceAccountToken: false
      securityContext: {runAsNonRoot: true, runAsUser: 65534, seccompProfile: {type: RuntimeDefault}}
      containers:
        - name: probe
          image: docker.io/library/busybox@sha256:73aaf090f3d85aa34ee199857f03fa3a95c8ede2ffd4cc2cdb5b94e566b11662
          command: ["sh", "-c", "cat /proc/version; uname -r"]
          securityContext:
            allowPrivilegeEscalation: false
            readOnlyRootFilesystem: true
            runAsNonRoot: true
            runAsUser: 65534
            capabilities: {drop: ["ALL"]}
          resources: {requests: {cpu: 100m, memory: 64Mi}, limits: {cpu: 500m, memory: 128Mi}}
YAML
kubectl -n labweaver-evaluation wait --for=condition=complete job/sandbox-probe --timeout=300s
kubectl -n labweaver-evaluation logs job/sandbox-probe   # 必须包含 4.19.0-gvisor
kubectl -n labweaver-evaluation delete job sandbox-probe --wait=true
```

handler 不可用或 `runsc` 缺失时必须失败关闭：不要把沙箱降级为节点默认运行时，也不要放宽
seccomp、no-new-privileges、drop caps 或只读 rootfs。

## 7. KubeVirt / VM 验证与 linux-nginx 材料链

KubeVirt 由 `70-install-kubevirt.yml`/`kubevirt` 角色安装，且显式 `useEmulation: false`（禁止软件模拟回退）；CDI scratch 绑定 local-path。KVM 能力由 preflight 的 `/dev/kvm` 与 `vmx`/`svm` 检查保证。

第 4 节 `verify` 已覆盖 KubeVirt 起停与控制台连接。平台 VM base 由 `platform_application` 以 CDI DataVolume 从锁定镜像导入：

```sh
kubectl -n labweaver-system get dv ubuntu-lab-base-v1-seed
kubectl -n labweaver-system get datasource ubuntu-lab-base-v1
```

linux-nginx 教材材料链：

```sh
python examples/linux-nginx/verify_material_contract.py --self-test
cargo test -p contracts --locked
```

- 运行链是 Ubuntu 24.04 VM + 只读 Probe profile（`deploy/versions.lock.yml` 的 `ansible_probe.playbook_profile` 为 `linux-nginx-probe-v1/playbook.yml`）。
- `material-manifest.json` 中受控 VM/Probe 条目为 `unbound`/`blocked-*`，其 `private://` locator 指向受控存储。只有在 Git 之外把批准版本与 SHA-256 绑定到同一 EvaluationRun 后该材料才可用。
- 负例必须显式失败并保留原始诊断：Nginx 停止/未监听、站点或页面不匹配、Probe 能力缺失、缺少镜像绑定、访问失败、超时、畸形事实。不得用模拟结果或更弱断言替代。

## 8. 残留与陈旧用户环境清理

统一规则：先枚举、再核对归属、只按精确命名空间/名称删除；禁止 `--all`、禁止无标签通配删除、禁止删除无法确认归属的对象。

用户环境（Environment 拥有，命名 `lw-env-*`）：

```sh
kubectl get ns -l labweaver.io/environment=true -o name
kubectl -n lw-env-<environmentId> get vmi,pod,svc,pvc
kubectl delete namespace lw-env-<environmentId> --wait=true
```

只删除已确认无在用工作负载、且与 Resource/Environment 记录一致的命名空间；命名空间删除会删除其 PVC。

node-debugger 残留（`kubectl debug node/...` 留下的 Pod）：

```sh
kubectl get pods -A -o name | grep 'node-debugger'
kubectl -n <namespace> delete pod <exact-node-debugger-pod> --wait=true
```

重复 Gateway：当前权威集合为 `labweaver-demo/public-gateway`（内部）、`labweaver-system/labweaver-public`（公网入口）、`keycloak-system/identity-gateway`、`harbor/harbor-gateway`。枚举后只删除带 LabWeaver 标签、且不属于以上四者、并确认无引用者：

```sh
kubectl get gateway -A
kubectl -n <namespace> get gateway <exact-name> -o jsonpath='{.metadata.labels}'
kubectl -n <namespace> delete gateway <exact-name> --wait=true
```

其他残留：已结束的 Job/Pod、孤儿 PVC/PV、失败/重复 Helm release secret。

```sh
kubectl get jobs -A
kubectl get pvc -A
kubectl get pv -o custom-columns=NAME:.metadata.name,CLAIM_NS:.spec.claimRef.namespace,STATUS:.status.phase
helm -n labweaver-system history labweaver
```

## 9. 故障排查

| 现象 | 可能原因 | 稳定诊断/检查 |
|---|---|---|
| 预检失败 | 清单分组/占位符/OS/SELinux/KVM/NFS 不符 | `PREFLIGHT_SCOPE_INVALID`、`IDENTITY_INVENTORY_SCOPE_INVALID`、"inventory must declare..."、`/dev/kvm` 缺失 |
| 打包被拒 | 脏树、profile 不符、工具身份不符、摘要缺失 | `LW_PACKAGE_INPUT_DIRTY`、`LW_PACKAGE_PROFILE_MISMATCH`、`LW_PACKAGE_TOOL_IDENTITY_MISMATCH`、`LW_PACKAGE_DIGEST_MISSING`、`LW_PACKAGE_MANIFEST_INVALID` |
| 打包缺配置 | 未导出 registry 或 tool 不在 PATH | `LW_PACKAGE_CONFIGURATION_MISSING`、`LW_PACKAGE_RUST_TOOLCHAIN_IDENTITY_MISMATCH` |
| 未带 `--infra`/`--yes` | xtask 拒绝危险或非显式操作 | `XTASK_INFRASTRUCTURE_REQUIRED`、`XTASK_CONFIRMATION_REQUIRED` |
| 非 Linux 控制器 | xtask 基础层操作仅限 Linux | `XTASK_INFRA_UNSUPPORTED_PLATFORM` |
| 身份基础层失败 | locator 路径/权限/哈希不符或 run id 非 `infra-` | `IDENTITY_CONFIGURATION_INVALID`、`IDENTITY_SECRET_LOCATOR_INVALID`、`IDENTITY_BOOTSTRAP_SECRET_INVALID` |
| 基础层 rollout 未就绪 | pod 卡在旧失败 revision 或镜像不符 | `PLATFORM_FOUNDATION_READBACK_INVALID`、`PLATFORM_FOUNDATION_INPUT_INVALID` |
| BuildKit 配置/VIP/CA 不符 | bundle、Harbor CA、DNS、Gateway 漂移 | `PLATFORM_BUILDKIT_CONFIGURATION_INVALID`、`PLATFORM_BUILDKIT_HARBOR_CA_MISMATCH`、`PLATFORM_BUILDKIT_READBACK_INVALID`、`PLATFORM_BUILDKIT_EGRESS_MODE_INVALID` |
| 应用部署失败 | 配置包键/边界、Harbor 项目、Keycloak、stream/bucket 冲突 | `PLATFORM_APPLICATION_CONFIGURATION_INVALID`、`..._KEYS_INVALID`、`..._HARBOR_PROJECT_INVALID`、`..._STREAM_CONFLICT`、`..._CONSUMER_CONFLICT`、`..._REQUIRED_PATH_MISSING` |
| Resource 部署失败 | 包 profile/镜像边界不符 | `LW_PACKAGE_PROFILE_MISMATCH`、`RESOURCE_APPLICATION_CONFIGURATION_INVALID`、`RESOURCE_APPLICATION_INPUT_INVALID` |
| 验证失败 | 节点集合/存储/Gateway/KubeVirt/Cilium 异常 | `NODE_SET_UNEXPECTED`、`VERIFY_EXECUTION_INPUT_INVALID`、`VERIFY_CLEANUP_FAILED` |
| OJ 无法调度 | RuntimeClass/handler/CRI-O/PID 上限缺失 | `OJ_RUNTIME_RUNTIMECLASS_INVALID`、`OJ_RUNTIME_HANDLER_CONFIG_INVALID`、`OJ_RUNTIME_RUNC_MISSING`、`OJ_RUNTIME_CGROUP_MANAGER_INVALID`、`OJ_RUNTIME_CRIO_INACTIVE` |
| 契约漂移 | 生成物与检出不符 | `LW_CONTRACT_DRIFT`（`cargo xtask contracts check`） |

## 10. 本轮无回滚、无备份与安全停止

- 本轮**没有**备份与回滚：`xtask backup`、`xtask rollback`、备份/清理 playbook 均已移除。不要指望任何命令恢复已删除数据。
- Helm 应用升级带 `--atomic`，失败只会回到上一 Helm revision；数据库迁移是前向的，应用回滚不等于数据库回滚。可用 `helm history`/`helm rollback <release> <revision>` 做应用层恢复，但不恢复数据。
- 共享基础组件（`labweaver-data`、`harbor`、`keycloak-system`）不随应用升级隐式清理；清理必须单独确认对象、归属和恢复路径。
- 安全停止：优先停止用户环境/VM 并等待执行后端确认释放。停止请求不等于已释放；未确认停止的计费保持待核实，不得标记为免费。
- 长时间或不可逆操作（删除基础层 PVC/PV、重建 Harbor/Keycloak、数据库清理）必须提前说明影响范围，并在变更窗口内由对应负责人确认后再执行。

## 11. v1 控制器实测补充（Issue #127）

以下为在 v1 共享集群控制器（本机 `v1-cp-63`）实际执行后必须遵循的环境事实。

### 11.1 控制器与私有输入

- 控制器的私有 Ansible 根目录是 `/var/lib/labweaver/v1-controller`（root-only），包含真实的
  `deploy/ansible/inventories/v1/hosts.yml`、`group_vars/all/{main,vault}.yml` 与
  `deploy/ansible/collections`。通过 `LABWEAVER_ANSIBLE_DEPENDENCY_ROOT=/var/lib/labweaver/v1-controller`
  让 xtask 使用私有 inventory，而 playbook/roles 仍来自当前检出。
- 固定控制插件在 `/opt/labweaver/venv`（ansible-core 2.18.6、kubernetes 34.1.0）。执行时必须把它加入 `PATH`；
  vault 口令文件位于 `deploy/ansible/inventories/v1/.vault-password`。
- 节点 SSH 使用 `labweaver-deploy` 用户与 `.private/v1-deploy-controller-key`；仓库内
  `inventories/v1/hosts.yml` 若仍指向 `root`/`/root/.ssh/id_rsa` 需要修正。
- 以 root 在 wzh 检出中运行 xtask 前，先执行
  `git config --global --add safe.directory /home/wzh/LabWeaver`，否则 `cargo xtask package` 读取
  Git 身份会失败。

### 11.2 镜像打包必须使用集群内 BuildKit

- `cargo xtask package` 会校验 BuildKit 版本与锁定值（`v0.31.1`）。本机 Docker 自带 v0.33.0，不满足。
  正确做法是把集群内 rootless BuildKit 端口转发到本地并建立 remote builder：

  ```sh
  kubectl -n labweaver-build port-forward svc/buildkit 1234:1234   # 需要在整个打包期间保持
  CERTS=/var/lib/labweaver/.private/v1/platform-buildkit/build-executor-client
  docker buildx create --name bk-local --driver remote \
    --driver-opt "cacert=$CERTS/ca.crt,cert=$CERTS/tls.crt,key=$CERTS/tls.key,servername=buildkit.labweaver-build.svc" \
    tcp://127.0.0.1:1234
  docker buildx use bk-local
  ```

- 打包需要 `LABWEAVER_KUBECONFIG=/etc/kubernetes/admin.conf`、`LABWEAVER_PLATFORM_REGISTRY=harbor.lab.lan`，
  且源码树干净（含 untracked）。
- **必须传 `LABWEAVER_BUILD_PROXY=http://49.52.27.95:7897`**：构建步骤（例如 agent-service 的
  `npm pack @anthropic-ai/claude-code-linux-x64`）在 BuildKit 的 Pod 内出网，只有该变量会把它作为
  `HTTP_PROXY/HTTPS_PROXY` 构建参数注入；缺省时步骤无代理，构建会以
  `BuildKit platform image build failed … npm pack` 失败（Pod 本身能到代理，但构建步骤拿不到）。
  另外 `npm pack` 产物必须与 `deploy/versions.lock.yml` 的 `claude_code_linux_x64_sha512` 一致。
- root 的 `~/.cargo/config.toml` 若启用了 sccache，长时间打包可能遇到
  `Failed to read response header`：以 `RUSTC_WRAPPER= SCCACHE_DISABLE=1` 运行可绕过缓存守护进程。
- 本机 `docker buildx create --driver remote` 的 endpoint 需要独占本地端口；若默认 1234 已被残留
  port-forward 占用，换端口（如 1235）重建 builder，`inspect` 显示 `inactive` 属正常（首次构建才探测）。
- Harbor 的基础镜像必须按 `deploy/versions.lock.yml` 的原 digest 存在；若某个 `base-*` 仓库的 digest 漂移，
  用 `docker buildx imagetools create` 经校园代理重新镜像（保留原 digest），例如：

  ```sh
  HTTPS_PROXY=http://49.52.27.95:7897 HTTP_PROXY=http://49.52.27.95:7897 \
  NO_PROXY=harbor.lab.lan,localhost,127.0.0.1 \
  docker buildx imagetools create \
    --tag harbor.lab.lan/labweaver-system/base-rust-builder:1.97.1 \
    docker.io/library/rust:1.97.1-bookworm@sha256:14bc9c5966e7b3a385794b3d5389a8765668342025fbcc7b2e3d2866ac4bd8c3
  ```

- `platform_application` 还会校验 `platform_container_images` 里每个基础镜像的**原 digest** 已在 Harbor
  项目内（Agent 的 seed 目录不能声称项目里不存在的镜像）。部署本身不做镜像复制，操作者必须先按
  `platform_container_images` 的 `name:tag` 建仓并镜像同一 digest，例如：

  ```sh
  HTTPS_PROXY=http://49.52.27.95:7897 HTTP_PROXY=http://49.52.27.95:7897 \
  NO_PROXY=harbor.lab.lan,localhost,127.0.0.1 \
  docker buildx imagetools create \
    --tag harbor.lab.lan/labweaver-system/rust:1.97.1-bookworm \
    docker.io/library/rust:1.97.1-bookworm@sha256:14bc9c5966e7b3a385794b3d5389a8765668342025fbcc7b2e3d2866ac4bd8c3
  docker buildx imagetools create \
    --tag harbor.lab.lan/labweaver-system/distroless-cc:nonroot \
    gcr.io/distroless/cc-debian12:nonroot@sha256:66aa873a4a14fb164aa01296058efd8253744606d72715e45acface073359faa
  ```

  缺失时部署以 `PLATFORM_APPLICATION_CONTAINER_IMAGE_DIGEST_ABSENT` 失败，不会留下只有名字可用的 seed 目录。

### 11.3 BuildKit 出网

- 新模板默认 `platform_buildkit_egress_mode: open`。若 `92-platform-buildkit` 因集群 CoreDNS 已存在
  非本 role 写入的 `harbor.lab.lan` 记录而报 `PLATFORM_BUILDKIT_CLUSTER_DNS_AMBIGUOUS`，可直接把
  `labweaver-build` 命名空间内的 `NetworkPolicy/buildkit` 与 `CiliumNetworkPolicy/buildkit-dependency-egress`
  的 egress 收敛为开放形态（DNS + 全部出网），与 `open` 模板一致。

### 11.4 沙箱运行时与 GPU

- sandbox 能力的节点必须运行 containerd（见 §6）；CRI-O 上 `RuntimeClass` 只会让应用容器进入
  gVisor，Pod sandbox 仍留在节点默认运行时，因此一次性负载必然以 `cannot load sandbox` 失败。
- 切换运行时后节点上会残留旧 CRI 的容器进程（systemd `crio-*.scope`）。它们会占住 hostPath 上的
  socket/端口（例如 `cilium-envoy` 的 `/var/run/cilium/envoy/sockets/*.sock` 导致新 Pod
  `unable to bind domain socket … errno=98`）。清理方式：`systemctl list-units --type=scope --all |
  grep -o 'crio-[a-f0-9]*\.scope' | xargs -r systemctl stop`，随后重启对应 DaemonSet 的 Pod。
- 切换运行时后若出现跨节点 Pod 流量中断（同节点与节点间 ICMP 正常、`cilium-dbg status` 报
  `Cluster health: … reachable` 下降），按顺序处理：`kubectl -n kube-system rollout restart ds/cilium`、
  重新施加 `playbooks/50-install-network.yml`、必要时先 `drain` 再重启该节点；恢复后
  `cilium-dbg bpf ipcache list` 中远端 Pod IP 必须带 `tunnelendpoint=<节点 IP>`。
- NVIDIA device plugin 需要通过 `runtimeClassName: nvidia` 运行才能看到设备，集群需要
  `RuntimeClass/nvidia`（handler `nvidia`）；containerd 侧由 `sandbox_runtime` 在存在
  `/usr/bin/nvidia-container-runtime` 的节点上生成同名 runtime 表。
- 仅 worker-97 的 V100 会以独占 `nvidia.com/gpu` 上报；time-slice 本轮不验证；worker-158 的 P40
  驱动/库版本不匹配，修复需要重载模块或重启节点，且会中断其 NFS 服务，因此必须排在镜像推送之后。

### 11.5 每实验评估 runner 镜像

- 控制器 `containerBuild.runnerDockerfilePath=evaluation/Dockerfile`；build-executor 配置
  `executor.serviceImage` 必须指向平台 evaluation-service 镜像（含 `/usr/local/bin/labweaver-service`），
  由打包产物 digest 填充。实验 `evaluation/Dockerfile` 使用
  `FROM ${LABWEAVER_SERVICE_IMAGE} AS labweaver-service` 再 `COPY --from=labweaver-service`。

### 11.6 公网域名（唯一用户入口）

- 公网入口是 `https://portal.labweaver.2018wzh.top`（`public_ingress` role，`portal-public` 路由
  `/api`、`/auth`、`/connect` → access-service，`/` → web；apex 由 `portal-apex-redirect` 301 到 portal）。
- 身份栈使用公网名 `keycloak.labweaver.2018wzh.top`：`identity_hostname` 是单一真源，证书 SAN、
  内部 Gateway、dnsmasq 记录都由它派生；`identity_public_address` 打开后 role 会额外经公网入口校验
  issuer。access-service 的 `oidc.issuer`/`redirect_uri`/`allowed_origins` 与 realm client 的
  `redirectUris`/`webOrigins` 必须同步为公网 origin，否则登录会在回调处失败。
- 集群内 Pod 通过 values 的 `hostAliases`（公网 Keycloak 名 → `keycloak-internal` Service ClusterIP，
  即 `internalIdentityProxy`）访问公网 issuer；`internalIdentityProxy.hostname` 也必须改为公网名，
  否则 Pod 侧 TLS 校验会失败。
- 改 issuer 后必须用 `tools/prepare_platform_access_seed.py --issuer <公网 issuer>` 重新生成 access
  seed（`access.actors` 以 issuer 为键），否则所有 actor 都无法解析。
- 控制器的 Keycloak 管理面走**公网入口**：`identity-gateway` 的 listener 仍只声明
  `keycloak.labweaver.internal`，其 `labweaver-keycloak-tls` 证书的 SAN 只有公网名，因此经内部 VIP
  校验必然失败；公网入口（`--resolve <公网名>:443:<公网 IP>`）由 public Gateway 提供 Let's Encrypt
  证书。`platform_application` 只导入**单张**证书到 kcadm 的隔离 truststore（`keytool -importcert`），
  所以 `platform_application_keycloak_ca_file` 必须指向 LE 根（`ISRG Root X1`，控制器上为
  `/etc/ssl/certs/ISRG_Root_X1.pem`），`platform_application_keycloak_server`/`keycloak_hostname`
  用公网名、`keycloak_endpoint_address` 用公网 IP；否则在
  `Authenticate retained Keycloak administration` 处失败。
- **对象存储的上传 URL 也必须用公网 origin**：`control-service` 的 `object_store.endpoint` 同时决定
  服务端读取和**浏览器直传的 presigned URL**。用内部名（`https://portal.labweaver.internal/`）时浏览器
  解析不到该主机，包上传会以 `UPLOAD_OBJECT_FAILED` 失败（页面上逐文件"失败"）。改为
  `https://portal.labweaver.2018wzh.top/`（path-style 下 bucket 就是 `/labweaver-artifacts` 这个
  nginx 代理前缀，`proxy_set_header Host $http_host` 保留签名用的 Host），并把该主机的
  `hostAliases` 指向公网入口 Gateway VIP（`10.99.0.140`）、`portal-ca.pem` 换成 LE 根
  （公网入口用 `labweaver-public-wildcard` 证书）。

### 11.7 模型服务可达性（authoring 与 Work 模板生成的前提）

- v1 的模型服务是宿主机上的 ollama（`labweaver-llm` 命名空间的 `ollama` Service 用手工 Endpoints
  指向 `49.52.27.63:11434`）。**Pod 直连节点 IP 会被 Cilium 归类为 `host` entity**：即使
  NetworkPolicy 的 `ipBlock` 写了 `49.52.27.63/32:11434`，策略也不会命中，authoring 的 AgentRun 会以
  `LW_PROVIDER_UNAVAILABLE` 失败（`sandbox` 里的 Claude Code 既取不到模型也取不到包对象）。
- 可用的形态是**集群内代理**：在 `labweaver-llm` 里跑一个 TCP 转发 Pod（本环境用 nginx `stream`
  转发到 `49.52.27.63:11434`，镜像取 Harbor 的 `base-web-runtime`，需要同命名空间的 Harbor
  pull secret）并暴露 ClusterIP Service `ollama-proxy:11434`。sandbox 的每次尝试只允许
  `NetworkPolicy`，而它对 host entity 无效；代理是**普通 Pod**，所以 ipBlock 能命中。

  ```yaml
  # ConfigMap ollama-proxy-config (namespace labweaver-llm), key nginx.conf:
  #   worker_processes 1; error_log /dev/stderr info; pid /tmp/nginx.pid;
  #   events { worker_connections 512; }
  #   stream { server { listen 11434; proxy_pass 49.52.27.63:11434;
  #                     proxy_connect_timeout 5s; proxy_timeout 1800s; } }
  # Deployment ollama-proxy: 1 replica, image
  #   harbor.lab.lan/labweaver-system/base-web-runtime@sha256:42a7d7f2ee23e9f5a1dcdf3647ba5c585bbd18f79e79cd817e70e8cd61c55779
  #   (mount the ConfigMap at /etc/nginx/nginx.conf, imagePullSecrets:
  #    harbor-labweaver-system-pull 复制到该命名空间)
  # Service ollama-proxy: ClusterIP, port/targetPort 11434
  ```

  需要同时改三处：`agent-service-config/anthropic-base-url` =
  `http://ollama-proxy.labweaver-llm.svc.cluster.local:11434`、
  values 的 `network.externalServiceEndpoints` 加 `10.0.0.0/8:11434`、
  `agent-service-config` 的 `sandbox.allowed_egress` 加 `10.0.0.0/8:11434`；角色的
  `platform-model-egress`/`authoring-model-egress` 两条 `CiliumNetworkPolicy` 再按
  `platform_application_model_namespace` 放行该端口。
- 这条路径必须显式配置，不得把模型缺失降级为 Mock 或更弱的生成目标。
- **预算也要按本机模型调**：单个请求可能耗时分分钟，`web/e2e/support/live.mjs` 的策略默认
  `timeoutMilliseconds=120000`、`maxCostMicrousd=1000000`（1 USD）会让 CLI 以
  `error_max_budget_usd` / 超时结束（沙箱尝试 `exit_code=1`、`LW_AGENT_SANDBOX_FAILED`）。
  `tools/user_acceptance.py` 现在默认下发 `LABWEAVER_E2E_LLM_TIMEOUT_MS=900000` 与
  `LABWEAVER_E2E_LLM_MAX_COST_MICROUSD=50000000`，两者都可用环境变量覆盖。

### 11.8 已知阻塞
- **服务身份缺 scope-mapping（已定位并临时修复，角色本身有缺陷）**：authoring 的失败链最终落到
  `agent.authoring.sandbox.stage_failed`（`failure_stage: sandbox.resource.create`）与
  `auth.service_token.failed`（`failure_stage: token_audience_validation`、
  `error_kind: Client(Token(TokenAudienceRejected))`）：agent-service 换到的服务令牌 `aud` 只有
  `labweaver-environment`、`scope` 只有 `profile email`，因此 resource-service 调用被拒。
  根因是 `identity_foundation` 的 provisioning Job **渲染失败**：
  `Provision the LabWeaver service OIDC clients and roles` 以
  `yaml.parser.ParserError … line 1315/1317` 失败，所以 `identity_service_role_assignments`
  的 client scope-mapping 从未写入 Keycloak（service account 只有 `default-roles-workloads`）。
  临时修复：按角色的 `identity_service_role_assignments` 逐条补齐
  `/clients/{caller}/scope-mappings/clients/{target}`（46 条 user mapping + 7 条 scope mapping），
  修复后 agent 令牌变为 `aud: [labweaver-resource, labweaver-environment]`。角色模板仍需 owner 修。
- **authoring sandbox 的资源申请需要人工/管理员批准**：沙箱尝试会先 POST 一条 `task` 资源申请
  （`resource.resource_requests`，`state=reviewing`），随后 `claim_after_approval` 最长等待
  `sandbox.wall_time_seconds`（3600s）。平台当前**不会**自动批准该内部申请，
  `lab` 旅程因此会长时间停在等待；批准入口是 BFF `POST /api/v1/resource-requests/{id}/approve`
  （需要管理员会话 + `X-CSRF-Token` + `Origin`，body 需 `expectedRevision`/`providerBinding`/`resources`/`durationSeconds`/`reason`）。
  这是产品决策点：要么给内部 authoring 任务预授权，要么旅程显式批准。
- **会话 TTL 必须覆盖验收旅程时长**：`access-service` 的 `browser.session_ttl_seconds`/`session_idle_ttl_seconds`
  默认 1800（30 分钟），而 `lab` 旅程（上传 + 两次 authoring + 候选构建 + VM/控制台）实测超过 30 分钟，
  中途所有 API 轮询开始返回 `401 {"diagnosticCode":"LW_AUTH_SESSION_REJECTED"}`，旅程以
  `LAB_EXPERIMENT_AGENT_RUN_STATUS_FAILED:401` 失败。v1 私有配置已把两者提到 14400（4 小时）；
  更长的旅程需要同步调大，或由前端实现会话续期。
- **本地模型的候选 JSON 是本轮最后的阻塞**：沙箱内 CLI 能以 `exit_code=0` 完成
  （`terminal_reason: completed`，`result.json` 135–382 KiB），但 runtime 报
  `agent.llm.candidate_parse_failed`，其完整字段为
  `diagnostic_code=LW_EVIDENCE_INVALID`、**`error_kind=SchemaInvalid`**、`retryable=true`：
  即**模型输出的候选 JSON 不满足所审阅的 schema**（不是提取失败）。契约把 `max_schema_repairs`
  限制为 **≤ 2**（`crates/contracts/src/authoring.rs:172`），不能靠加大修复次数解决；已实测
  `qwen3.6:27b`、`qwen3.6:35b`、`glm-4.7-flash:latest` 三种本地模型均为同一形态。
  处置方向：强化候选提示/放宽 schema，或接入满足该 schema 的模型——属于产品决策，
  不得用 Mock 或放宽断言替代。实测取到的最新一代模型输出（CLI `result.subtype=success`、
  `is_error=false`，即 CLI 成功）是：

  ```json
  {"evaluation":{"apiVersion":"evaluation.labweaver.io/v1","kind":"EvaluationSpec","metadata":{}},"runnerBuildRecipe":{"mode":"package"}}
  ```

  即模型给出的是**结构合理但字段不全**的候选（缺必填字段/空 metadata），因此是提示与 schema
  严格度的问题，而不是链路或权限问题。已实测的模型矩阵（同一旅程、同一 schema）：

  | 模型 | sandbox CLI | runtime 结果 |
  |---|---|---|
  | `qwen3.6:27b`（部署原值） | `exit 0`，`result.json` 135–382 KiB | `SchemaInvalid` → `LW_EVIDENCE_INVALID` |
  | `qwen3.6:35b` | `exit 0` | 同上 |
  | `glm-4.7-flash:latest` | `exit 0`，`subtype=success` | 同上（候选字段不全） |
  | `ornith:35b` | `exit 1` | `ExecutionFailed` → `LW_PROVIDER_UNAVAILABLE` |

  实验后已把 `anthropic-model` 恢复为部署原值 `qwen3.6:27b`。
- **`ToolDenied` 的精确原因：`--bare` 把内置工具集收敛到 3 个**。runtime 传给 CLI 的是
  `AUTHORING_TOOLS = "Bash,Edit,Glob,Grep,Read,Write"`（canonical policy
  `AUTHORING_TOOL_POLICY_CANONICAL_JSON` 也是这 6 个），但沙箱内 CLI 的 init 事件只列出
  `["Bash","Edit","Read"]`。在沙箱镜像内直接对照（同一 `--tools` 取值，仅切换 `--bare`）：

  ```sh
  claude --print --output-format stream-json --tools "Bash,Edit,Glob,Grep,Read,Write" ...
  # -> "tools":["Bash","Edit","Glob","Grep","Read","Write"]     6 个

  claude --bare --print --output-format stream-json --tools "default" ...
  # -> "tools":["Bash","Edit","Read"]                          3 个
  ```

  即 **`--bare`（内部置 `CLAUDE_CODE_SIMPLE=1`）会把内置工具集收敛为 3 个**，与 `--tools`
  取值无关；模型按提示去写文件时调用 `Write`/`Glob`/`Grep` 即被拒，
  `error_kind=ToolDenied`（`retryable=false`），最终落在 `LW_PROVIDER_UNAVAILABLE`。
  修法二选一：authoring 调用去掉 `--bare`（会重新启用 hooks/LSP/插件/CLAUDE.md 自动发现，
  与 `--bare` 的初衷冲突，需安全评审），或把 canonical tool policy 收敛到 CLI 实际支持的
  3 个工具（同步 canonical JSON 与 sha256 断言）。属产品/契约决定，未擅自修改。
- KubeVirt 控制面（virt-api/virt-controller/virt-operator）长期 CrashLoop（报
  `dial tcp 10.96.0.1:443: i/o timeout`），因此 linux-nginx VM+Probe 验收需要先修复 KubeVirt 控制面。
- worker-158 的 P40 驱动与库版本不匹配，需要重载模块或重启节点后才能作为 GPU 提供方。
- **dispatch worker 是串行且按 `created_at` 先到先处理**：一次只跑一个 reserved dispatch
  （`agent.dispatch.claimed` 后要等它完成），且 claim 的 `ORDER BY created_at` 决定顺序。被中断的旧
  run 会留下 `pending`/`preparing` 的 dispatch，它们会**先**占用 worker，使新 run 长时间排队
  （实测：三条陈旧 dispatch 各等待其沙箱审批，新 run 排队 20+ 分钟）。验收前应确认队列为空，
  或先把陈旧 run 取消；不要靠改数据库绕过。
- **CLI 的预算来自 run 的 policy 快照**：`--max-budget-usd` 取自 dispatch 里的 policy，而不是当前
  项目策略，所以调大 `LABWEAVER_E2E_LLM_MAX_COST_MICROUSD` 后**旧的 pending dispatch 仍是旧预算**
  （实测仍以 `error_max_budget_usd`、约 1.09 USD 结束）。新 run 才会带上新预算。
- **authoring AgentRun 失败且无 sandbox Job（未解决，需 owner 判断）**：三条旅程都在“发布包”这一步以
  `LW_PROVIDER_UNAVAILABLE` 失败，且每条 run 的两个 track 各报一次。已实测的边界：
  - `agent.agent_run_dispatches` 有本次记录且状态为 `prepared`（`bind_prepared_dispatch` 由
    `agent-service` 写入），说明 agent-service 已消费 dispatch、读过全部包对象并生成 egress envelope；
  - `labweaver-authoring` 内 5 分钟 2 秒粒度 watch 捕获 **0 个 Pod**；全集群 60×2s watch 也没有任何本次
    run 的 Job/Pod；`agent.authoring_sandbox_attempts` 无行；
  - agent-service 日志里没有 `agent.dispatch.completed`、也没有 `preparation_failed`，Pod 重启数为 0，
    即失败发生在 `bind_prepared_dispatch` 之后的 `execute_reserved_dispatch` 段，且该段**没有稳定诊断输出**
    （这是需要一并修的可观测性缺口：worker 的 `Err` 直接经 `select!` 冒泡，既不落日志也不落事件）；
  - 该段所需的 RBAC 已单独验证（补 `list/watch` 后重跑仍失败，随后还原）；
  - 已排除的假设（都可复核）：agent-service 的 `minio-ca.pem` 与 MinIO 服务端 `ca.crt` 指纹一致、
    与 control-service 的 `minio-access-key/secret-key` 指纹一致；`SandboxAuthoringProcess` 的
    `verifies_identity_in_execution() == true`，所以不会走 `version()` 那条恒返回 `Unavailable` 的分支；
    track work item 在创建后约 1 秒即 `failed`，`execution_request`/`execution_receipt` 均为空，说明失败在
    尝试落地之前，且 `agent.agent_run_dispatches` 已是 `prepared`。
  处置：按 §12「产品缺陷」改源码后重新打包部署，并补上该段的失败诊断；不得以 Mock 或降级断言绕过。

## 12. 用户验收（模拟真实用户操作）

验收入口是 `tools/user_acceptance.py`，它把「可重复」落在三个地方：集群与公网前提的 `preflight`、
按旅程驱动的 Playwright 运行，以及落到 `artifacts/acceptance/<run-id>/` 的证据与 `summary.json`。

前提（root 私密 → 运行者可读的 0600 副本）：

```sh
sudo install -d -m 700 -o wzh -g wzh /home/wzh/.private/labweaver-acceptance/credentials
for role in teacher student admin; do
  sudo install -m 600 -o wzh -g wzh \
    /var/lib/labweaver/.private/v1/platform-application/keycloak-user-platform-$role-password \
    /home/wzh/.private/labweaver-acceptance/credentials/$role.password
done
```

预检与运行（证据目录必须可写；`artifacts/` 属 root，必要时用 root 运行或指定可写目录）：

```sh
python3 tools/user_acceptance.py preflight \
  --base-url https://portal.labweaver.2018wzh.top \
  --evidence-dir /home/wzh/LabWeaver/artifacts/acceptance

python3 tools/user_acceptance.py run \
  --base-url https://portal.labweaver.2018wzh.top \
  --run-id <run-id> --journeys lab,work,admin --lab xv6 \
  --evidence-dir /home/wzh/LabWeaver/artifacts/acceptance
```

旅程映射（与工具内写死的一致）：

| key | project | spec | `--grep` 标题 | 额外 env |
|---|---|---|---|---|
| `lab` | teacher | `web/e2e/teacher/lab-experiment.live.spec.mjs` | `student completes a published lab experiment through the browser terminal` | `LABWEAVER_E2E_LAB=<xv6|cuda>` |
| `work` | student | `web/e2e/student/sprint2-flow.live.spec.mjs` | `student provisions a Work environment, configures it, and releases its capacity` | `LABWEAVER_E2E_REAL_PROVIDER=1`、`LABWEAVER_E2E_SECURITY_BASE_IMAGE=<digest>` |
| `admin` | platform-admin | `web/e2e/platform-admin/resource-approval.live.spec.mjs` | `platform administrator approves a real resource request and reads back its lease and charges` | — |
| `authoring` | teacher | `web/e2e/teacher/authoring.live.spec.mjs` | `teacher authors an independent project and publishes its complete experiment package` | — |

证据目录布局：

```
artifacts/acceptance/<run-id>/
  summary.json                 # run_id、base_url、git_commit、package_manifest、helm_revision、bundle_sha256、逐旅程 status/diagnostic、起止时间
  .credentials/                # 0700；每个 0600 口令文件（仅本次运行的副本）
  <journey>/                   # playwright-report/{report.json,index.html} 与 test-results/**
  <journey>.stdout.log         # 原始 stdout/stderr
```

失败分类与处置（不得放宽断言）：

- 集群/沙箱/镜像/模型等前提缺失 → 回到 §6 与 §3 补齐后重跑。
- 身份与路由配置错误（issuer、redirect、allowed_origins、realm client、hostAliases）→ 回到 §11.6 修正并重跑部署。
- 产品缺陷（页面死路、错误不可读、假进度、刷新重试导致重复资源）→ 改源码与受影响测试，重新打包部署后重跑。
- 已知但不阻塞的易用性打磨项 → 记入 §11.7，附截图与稳定诊断码。
