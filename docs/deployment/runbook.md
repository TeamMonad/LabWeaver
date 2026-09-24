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

### 6.1 本轮复验（2026-09-24，A2 判据）

在两条 worker 上各跑一次 `runtimeClassName: labweaver-sandbox` 的探针 Job（镜像用 manifest 里 pin 的
`base-rust-builder@sha256:14bc9c59…`，并带 `imagePullSecrets: harbor-labweaver-system-pull`，否则 worker
拉不动 Harbor 私有仓库）：

```sh
kubectl --kubeconfig .private/kubeconfig-v1-admin.conf apply -f - <<'YAML'
apiVersion: batch/v1
kind: Job
metadata: { name: sandbox-probe-a2 }
spec:
  backoffLimit: 0
  template:
    spec:
      runtimeClassName: labweaver-sandbox
      restartPolicy: Never
      imagePullSecrets: [{ name: harbor-labweaver-system-pull }]
      containers:
        - name: probe
          image: harbor.lab.lan/labweaver-system/base-rust-builder@sha256:14bc9c5966e7b3a385794b3d5389a8765668342025fbcc7b2e3d2866ac4bd8c3
          command: ["/bin/sh","-c","cat /proc/version"]
YAML
```

实测结果：`v1-worker-158` 与 `v1-worker-97`（后者用 `nodeName` 固定）都 `succeeded=1`，日志均为
`Linux version 4.19.0-gvisor #1 SMP …`；`kubectl get runtimeclass` 只剩 `labweaver-sandbox` 与 `nvidia`，
被取代的 `labweaver-oj` 已不存在。探针 Job 用完即删。

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
- **编译器缓存 wrapper 会让打包整体失败（已实测更正）**：本机在 `/home/wzh/.cargo/config.toml` 里把
  `build.rustc-wrapper` 指向 `/home/wzh/.cargo/bin/sccache`。cargo 会**按目录向上**读配置，所以在
  `/home/wzh/LabWeaver` 下的任何一次构建（**包括以 root 运行的 `xtask package`**）都会命中它；
  该 sccache 服务端一旦不可达，cargo 连探测 `rustc -vV` 都失败，打包在 0.2 秒内以
  `process didn't exit successfully: …/sccache … rustc -vV (exit status: 2)` +
  `sccache: Failed to read response header` 结束。
  实测**无效**的绕过：`RUSTC_WRAPPER= SCCACHE_DISABLE=1`（cargo 把空值当未设置，仍回落到目录配置）、
  以 root 重启 sccache（用 `sudo -i -u root bash -lc 'sccache --start-server'` 起的服务端会随该 shell 退出，
  下一次调用仍是 `Failed to read response header`）、或删掉 `/root/.cargo/config.toml`（该文件本就不存在）。
  实测**有效**的做法：打包期间把那行注释掉，结束再还原——
  ```sh
  CFG=/home/wzh/.cargo/config.toml; cp "$CFG" /tmp/cargo-config-wzh.bak
  python3 - "$CFG" <<'PY'
  import sys; p=sys.argv[1]; t=open(p).read()
  open(p,'w').write(t.replace('rustc-wrapper = "/home/wzh/.cargo/bin/sccache"',
                              '# rustc-wrapper disabled for this packaging run'))
  PY
  # …运行 cargo xtask package…；随后 cp /tmp/cargo-config-wzh.bak "$CFG" 还原
  ```
  验证方式（与 cargo 的探测完全一致）：
  `sudo -i -u root env PATH=/usr/local/bin:/usr/bin:/bin bash -lc 'cd /home/wzh/LabWeaver && cargo check -p task-execution'`
  ——关掉 wrapper 后应在数十秒内 `Finished … profile`。
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
- **预算必须按「一次 authoring 会话」而不是「一次请求」来设**。authoring 的 provider 调用由
  `AUTHORING_MAX_TURNS = 60` 限次（`services/agent-service/src/claude_code.rs:2933`），每回合都会
  重发审阅过的提示词，`enforce_budget` 用**单次会话累计用量**比对项目策略的四个上限
  （`max_input_tokens`/`max_output_tokens`/`max_requests`/`max_cost_microusd`，`claude_code.rs:2839-2860`），
  任一越界即 `BudgetExceeded` → `LW_RESOURCE_EXHAUSTED`（CLI 本身仍以 `exit 0` 结束，沙箱尝试
  记录为 `terminal|exit_code=0`，因此**必须看 worker 事件而不是沙箱退出码**）。实测：早期
  `maxRequests=8/24`、`maxInputTokens=200_000` 的验收策略在本机 27B 模型下会在 2–3 分钟内被
  `BudgetExceeded` 打断。
  验收预算现在只有一处定义：`web/e2e/support/live.mjs` 的 `policyFor` 默认
  `maxInputTokens=20000000`、`maxOutputTokens=5000000`、`maxRequests=200`、
  `maxCostMicrousd=500000000`、`timeoutMilliseconds=900000`，全部可用
  `LABWEAVER_E2E_LLM_MAX_INPUT_TOKENS` / `_MAX_OUTPUT_TOKENS` / `_MAX_REQUESTS` /
  `_MAX_COST_MICROUSD` / `LABWEAVER_E2E_LLM_TIMEOUT_MS` 覆盖；`tools/user_acceptance.py` 不再重复
  定义这些默认值，直接透传调用方环境。`real-experiment` spec 只保留 `maxTransientRetries=0`
  这一条刻意的覆盖（真实链路必须证明单次未重试的尝试）。
- **实测用量（据此定上限，不要凭感觉调）**：从对象存储读回沙箱的 `result.json`
  （`labweaver-artifacts` 桶，`problem-packages/authoring-sandbox/<attempt-id>/result.json`，
  内容是按行分隔的 stream-json，取最后一条 `type=result` 的 envelope；TLS 用
  `agent-service-secrets/minio-ca.pem` 校验、把 `minio.labweaver-data.svc` 指到 port-forward
  的 127.0.0.1 即可在宿主机读），两次真实 authoring 尝试为：
  | attempt | CLI subtype | turns | input tokens | output tokens | cost USD |
  |---|---|---|---|---|---|
  | `01a0d2ebf7…`（environment） | `success` | 2 | 211,022 | 2,754 | 1.12 |
  | `01a0d2ee-c215…`（evaluation） | `success` | 7 | 756,663 | 3,915 | 3.88 |
  即**单回合输入约 10 万 token**（materials 整体进提示词），所以 `maxInputTokens=200_000`
  在第二回合就会被打穿（这正是「CLI `exit 0` 但 runtime 报 `BudgetExceeded`」的成因）；
  `maxRequests`/`maxOutputTokens`/`maxCostMicrousd` 从来不是瓶颈（turns ≤ 7、output ≤ 4k、
  cost ≤ 4 USD）。真正的边界是 CLI 的 `--max-turns 60` 与墙上时间，因此上限设成
  60 回合也吃不完的量级（20M / 5M / 200 / 500 USD）。
- **诊断缺口（待补）**：`enforce_budget` 只返回 `BudgetExceeded`，不记录是哪一个维度越界
  （`services/agent-service/src/claude_code.rs:2839`），日志里也没有 usage；本轮只能靠从对象存储
  读回 `result.json` 才能定性。建议后续在该分支补上越界维度与「观测值/上限」两个字段。
- **已修（部署态缺陷）：集群里的 NATS 用户凭据比权威权限表旧，导致 evaluation-service 无法发布 release 事件**。
  证据链：lab 旅程走到「发布」时报 `LAB_EXPERIMENT_PUBLICATION_FAILED:LW_EVALUATION_RELEASE_PUBLISH_UNAVAILABLE`
  （`control-service/src/messaging.rs:1017` 的 `evaluation.publish → DownstreamError::Unavailable`）；
  `evaluation-service` Pod 处于 `CrashLoopBackOff`，其启动期 outbox 反复报
  `Error: Process(Outbox(PublishRejected { subject: "labweaver.evaluation.release.published.v1",
  reason: "timeout", stage: "publish_ack" }))`，同时 NATS 服务端对同一 subject 记录
  **`Publish Violation`**（权限拒绝，而非网络问题）。解码集群里的
  `labweaver-system/evaluation-service-secrets/nats.creds` 得到 `pub.allow` 只有
  `{$JS.ACK.>, $JS.API.>, submission.freeze_requested.v1, submission.frozen.v1}`，
  而权威表 `tools/prepare_platform_foundation.py` 的 `NATS_USERS["evaluation-service"]` 有 9 个 subject
  （含 `labweaver.evaluation.release.published.v1`）；`nsc describe user` 显示**私钥库里的用户 JWT 是对的**
  （发布者与 subject 完全一致），说明**只有下发到集群的那份 creds 是旧的**。同一类问题还有
  `access-service` 缺 `labweaver.environment.instance.state_changed.v1` 订阅等。
  处置（已执行，逐服务）：用**权威 nsc store**（`/var/lib/labweaver/.private/v1/platform-foundation/nsc`）
  `nsc --all-dirs <store> generate creds --account WORKLOADS --name <svc> --output-file <tmp>` 重新生成，
  把内容写回 `labweaver-system/<svc>-secrets` 的 `nats.creds` 键与私有
  `platform-application/render-input/secrets/<svc>-secrets/nats.creds`，再 `kubectl rollout restart` 该 Deployment；
  `evaluation-service` 随即 `1/1 Running`（未再出现 Publish Violation）。
  操作注意：`nsc` 生成的 creds 是 `0600 root`，非 root 直接 `base64` 会读到空内容——必须用 `sudo base64`（本轮曾因此把 9 个
  `nats.creds` 键写空后立即用同一流程修复，最终校验每个键 1520–2300 字符且 JWT 段可解析）。
  根因（部署态）：NATS 用户凭据只在最初建基础时生成一次，之后权限表新增 subject 不会再刷新；
  重新建立基础需要重跑 `prepare_platform_foundation.py`（输出目录 create-once），因此这类「权限表新增 subject」
  必须显式刷新凭据并滚动服务，否则会出现「服务启动即崩、且只有权限违规日志」的隐性故障。
- **产品含义（成本/时延，非阻塞）**：单回合约 10 万输入 token 意味着 materials 是整体进提示词的，
  按真实云端模型计费时一次 authoring（数十回合）会显著计费并拖长首字时延。本轮验收用本机模型
  不受影响；若后续要用真实 provider，应把「按对象引用 materials」而不是整体内联作为优化项
  （属产品/性能范围，本轮不擅自改动提示词结构）。

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
  **已修复**：authoring 调用不再传 `--bare`（非 authoring 候选不调用任何工具，仍保持最小
  `--bare` 模式），`AUTHORING_TOOL_POLICY_CANONICAL_JSON` 同步记录 `"bare":false`；在沙箱镜像内
  实测同一环境下去掉 `--bare` 后 init 列出全部 6 个工具且 `--model $ANTHROPIC_MODEL` 正常解析
  （`qwen3.6:27b`），因此 6 工具策略与 CLI 能力重新一致。附带代价：去掉 `--bare` 会重新启用
  hooks/LSP/插件/CLAUDE.md 自动发现——沙箱 `/workspace` 为空且出网仅限 Harbor，实测无额外副作用。
- **`ToolDenied` 的第二条、也是真正的阻塞原因：流解析器把任何 `tool_use` 块判为拒绝**。
  `parse_stream_output` 在 assistant 消息里遇到 `tool_use` 直接返回
  `ClaudeCodeRuntimeError::ToolDenied`，而 authoring 提示词恰好要求模型读 `/materials`、写
  `/workspace`、用 Bash 跑命令——于是**只要模型真的用了工具，整次 authoring 就以
  `LW_PROVIDER_UNAVAILABLE` 失败**（这与工具是否可用无关，`--bare` 只是让它更早触发）。
  **已修复**：`tool_use` 块改为跳过（候选只取 assistant 的 `text` 块），真正被策略拒绝的工具
  仍通过 envelope 的 `permission_denials` 失败；新增回归测试
  `tool_use_turns_do_not_discard_the_final_candidate`（改前失败、改后通过）。
- **同一解析器的第三个拒绝点：工具结果回合**。CLI 在每次工具调用后回灌一个
  `type=user` + `content=[{"type":"tool_result",...}]` 的回合，而 `valid_synthetic_user_event`
  要求 `isSynthetic=true` 且内容全是 `text`，于是该回合被判 `ProtocolInvalid`
  → `LW_EVIDENCE_INVALID`（实测：`ToolDenied` 修掉之后紧接着出现的就是它）。
  **已修复**：新增 `valid_tool_result_user_event`，只接受**全部为 `tool_result` 块**的 user
  回合（真实用户消息带 `text` 块，仍被拒绝，注入防护不变）；同时让非 schema 的解析失败也
  记录同样的有界 `stdout_preview`，便于定位协议类拒绝。回归测试覆盖
  `tool_use` + `tool_result` 循环。
- **Work 环境失败的真实原因：候选里的 `provider_binding` 不是集群注册的那个**。模型只能
  从提示词里得知可用的 provider binding，而提示词此前**没有**任何 binding 列表，于是候选写的是
  示例包里的开发值 `kubernetes-work-local-hostpath`；环境服务只注册 `container-primary-v1`
  （`environment-service-config/providers.json`），`ProviderRegistry::resolve` 直接返回
  `LW_ENVIRONMENT_PROVIDER_UNAVAILABLE`（reconcile 第 1 步、`duration_ms: 0`）。
  **已修复**：`DeploymentFile.authoring_provider_bindings`（可选，默认空）经
  `ClaudeCodeRuntime::with_provider_bindings` 进入 authoring 提示词的
  `PLATFORM PROVIDER BINDINGS (authoritative)` 段，候选只能复用可用的 binding 名；v1 私有
  bundle 里设为 `[container-primary-v1]`。同时验收侧不再硬编码开发值：
  `tools/user_acceptance.py` 从 `environment-service-config/providers.json` 读出容器 provider 的
  `binding` 并以 `LABWEAVER_E2E_PROVIDER_BINDING` 注入旅程，`real-experiment.mjs` 还会把该值写回
  上传的示例包 manifest（示例包本身仍保留开发默认值）。
- **私有 render-input 与仓库里的 bundle manifest 不同步**：`deploy/config/platform-bundle-manifest.json`
  列出的 configmap/secret 比 `render-input/` 实际内容多，直接用它渲染会得到
  `LW_PLATFORM_BUNDLE_INPUT_INCOMPLETE`；按 render-input 目录结构生成 manifest
  （`configMaps`/`secrets` → 名称 → 键列表，namespace `labweaver-system`）后渲染正常
  （20 个对象）。改私有 bundle 输入后必须重新渲染并让 vars 文件指向新文件名。
- **更正（重要）：上面的「Pod DNS 全域失效」是探针假象，不是平台故障**。用 `busybox` 裸 Pod 直接
  探 `kube-dns` ClusterIP 与 CoreDNS Pod IP（`10.0.0.9`、`10.0.1.40`）都超时，是因为
  `labweaver-system` 等命名空间带默认拒绝的出口策略，裸探针不在放行集内；**同一时刻平台自身的
  集群内调用全部正常**：`agent.platform_image.seed_existing`、`artifact_store` 客户端初始化、
  authoring 沙箱拉取 Harbor 镜像并完成、`agent.agent_track_work_items.heartbeat_at` 持续刷新。
  结论：agent-service 所在 worker-97 的 Cilium 与 DNS 都健康；worker 卡在「claim 之后无事件」
  的真实原因是**每次运行耗时远长于验收轮询上限**（worker 一次只跑一个 reserved dispatch，
  `created_at` 先到先处理，队列里还有更早的 run），而不是网络或 datapath 故障。
  排查网络时应使用带平台标签的探针或直接观察平台自身的成功事件，不要用裸 Pod 判断。
- **历史记录（已被上面的更正取代）：控制平面节点的 Cilium datapath 卡住、Pod 内 DNS 全部超时**。
  证据链（本轮实测）：
  - 集群内探针（`labweaver-system` 与 `labweaver-authoring` 两个命名空间）里 `nslookup`/`wget` 对
    `kube-dns` 的 `10.96.0.10:53` 一律 `connection timed out; no servers could be reached`，
    对 `keycloak-internal.keycloak-system.svc` 也拿不到地址；同一时刻从宿主访问公网入口正常
    （`portal=200`、`/auth/login` 307），说明 Keycloak 本身健康、**故障在集群内网络**。
  - `kubectl -n kube-system get pods -l k8s-app=cilium`：`v1-cp-63`（控制平面）上的 agent 报
    `error in controller endpoint-938-regeneration-recovery: regeneration recovery failed`；
    `cilium-dbg endpoint list` 显示 **938 就是该控制平面节点的 host endpoint**，状态 `Disabled`、
    阶段 `regenerating`。重启该 agent 后仍是同一端点卡住（换 pod 名后错误依旧）。
  - CoreDNS 也因此受影响：曾有一个副本 `0/1 Running, 227 restarts`（日志
    `plugin/kubernetes: Failed to watch`），删掉重建后两副本 `1/1`，但**Pod 内 DNS 仍然超时**，
    证明问题在 datapath 而不在 CoreDNS 进程。
  影响：任何 in-cluster 服务调用（agent-service → Keycloak 的 service token/JWKS、worker 的
  对象存储与后续步骤）在 DNS 超时后长时间挂起，表现为「`agent.dispatch.claimed` 之后再无事件」。
  需要节点级恢复（控制平面节点重启或 Cilium datapath 重放），属于基础设施操作，本轮未执行。
  已尝试的非破坏性恢复（均未成功）：删除并重建崩溃的 CoreDNS 副本（两副本恢复 1/1，但 Pod 内 DNS
  仍超时）、重启该节点上的 Cilium agent（换 pod 名后同一端点仍报错）、
  `cilium-dbg endpoint regenerate 938`（状态在 `regenerating → not-ready → waiting-to-regenerate`
  之间循环，DNS 始终超时）。因此该端点需要节点级恢复才能回到 `ready`。
- **排查线索：受控 OIDC HTTP 客户端没有超时**。`auth::no_redirect_http_client`
  （`crates/auth/src/provider.rs:266`）只设置 `no_proxy`/`redirect(none)`/TLS 信任，**没有 `timeout`**；
  凡是用它发起的 OIDC 调用（service token 刷新、JWKS 刷新）在被网络黑洞吞掉时会**永久挂起**，与
  「claim 之后没有任何后续事件」的现象一致。已确认启动期 discovery 是成功的（Pod Ready），
  因此这只是候选原因，尚未定位到具体调用点。
- **未解决（当前阻塞）：dispatch 被 claim 之后 worker 不再前进**。已修掉前置缺陷（seed 解析、工具策略、
  provider binding、凭据）后，重启 agent-service 可见 `agent.platform_image.seed_existing`（两个 seed 已在
  目录中）与 `agent.dispatch.claimed`，随后**没有任何后续事件**：没有 `agent.track.started`、
  没有沙箱 Pod、没有新的 `agent.build_commands`、`environment-service` 日志无调用、
  `pg_stat_activity` 无阻塞查询、track 一直停在 `requested`（旧 run 则停在 `running` 且租约已过期）。
  即卡点在 claim 与 track 启动之间（dispatch 绑定/egress 输入物化一带），该段目前没有可区分阶段的日志。
  重启 worker 只能让它再 claim 一次并再次停住；`tools/user_acceptance.py preflight` 的
  `authoring_queue` 会以 `LW_ACCEPTANCE_AUTHORING_QUEUE_BUSY` 报出积压。
- **历史记录：authoring 成功后 worker 停在该 track 上**。证据链：authoring 沙箱
  尝试正常结束（`agent.authoring_sandbox_attempts` 的 `environment|terminal|exit_code=0`），
  但 `agent.agent_track_work_items` 的该 track 长期停在 `running`：`heartbeat_at` 只在 claim 后
  ~2 分钟更新过一次（`07:21:17` / `07:27:42`），`lease_expires_at` 随后过期且不再续租；同时
  ① 没有新的 `agent.build_commands` 行（构建命令从未发出）、② `environment-service` 日志自部署后
  无任何调用、③ 无沙箱 Pod、④ `pg_stat_activity` 无阻塞查询、⑤ agent-service 的最后一个
  `artifact_store` 事件与 claim 同一秒（`endpoint` 已 redaction）。
  重启 `deploy/agent-service` 只能让 worker 重新 claim 一次，随后再次停住；因此队列会稳定积压
  （`agent_run_dispatches` 的 `pending`），`tools/user_acceptance.py preflight` 的
  `authoring_queue` 检查会以 `LW_ACCEPTANCE_AUTHORING_QUEUE_BUSY` 报出。
  推断卡点在 authoring 之后、构建命令之前（候选物化/对象存储调用），尚未定位到具体代码行；
  需要 agent-service 侧更细的诊断（当前该步没有可区分阶段的日志）。
- **平台镜像 seed 解析失败的真实原因：accept 头不含 OCI index 类型**。`agent-service` 启动时
  解析 `platform_registry.seed_images`，`OciRegistryPublisher::resolve_tag` 只声明
  `application/vnd.oci.image.manifest.v1+json` 与 `application/vnd.docker.distribution.manifest.v2+json`；
  Harbor 对这类镜像返回 **OCI index**，accept 不支持 index 时直接以 404
  `MANIFEST_UNKNOWN: OCI index found, but accept header does not support OCI indexes` 拒绝，于是
  `rust-builder-v1`、`distroless-runtime-v1` 两个 seed 都失败（`LW_PLATFORM_IMAGE_SEED_FAILED`），
  目录里没有固定基础镜像，后续 authoring 无法解析基础镜像。对照实验：
  ```sh
  curl -sk -u "$ROBOT:$SECRET" -H 'Accept: application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json' \
    https://harbor.lab.lan/v2/labweaver-system/rust/manifests/1.97.1-bookworm
  # 404 MANIFEST_UNKNOWN: OCI index found, but accept header does not support OCI indexes
  ```
  **已修复**：accept 头补上 index/manifest-list 类型，tag 解析到 index 时**跟随一次**到
  `linux/amd64` 条目（保持 pin 的是单平台镜像），并新增 `index_platform_digest` 的选择/拒绝回归测试。
- **另一处易混点**：seed 失败的日志只带稳定诊断码，`cause` 字段不会出现在 JSON 日志里；已在
  `resolve` 失败分支补记 `error_kind`，便于区分凭据被拒、注册表不可达与媒体类型不支持。
- **A2 复核（当前实机状态）**：`kubectl get runtimeclass` 只有 `labweaver-sandbox`（handler
  `labweaver-sandbox`）与 `nvidia`，`labweaver-oj` 已不存在；两台 worker 各跑一次
  `runtimeClassName: labweaver-sandbox` + `nodeSelector` 的探针 Job（`busybox`，`cat /proc/version`），
  两者都输出 `Linux version 4.19.0-gvisor` 并在数秒内完成，证明 gVisor 运行时在 worker-97 与
  worker-158 上仍然可用；探针 Job 用完即删。
- **A3 复核（当前实机状态，rev 74）**：`helm -n labweaver-system status labweaver` = `deployed`
  （revision 74）；`deploy -l app.kubernetes.io/instance=labweaver` 的 **11/11** 个 workload 都带
  `labweaver.io/configuration-bundle-sha256` 注解，且取值唯一，等于本地 bundle
  `configuration-bundle-public-20260923s.yml` 的 `sha256sum`
  （`sha256:e3c5a25efdcc81edb81db6ad2561ecb827639e4a7bda5894e678c20277579e7f`；本轮验收目录里的
  `deployment-readback.txt` 记录了逐 workload 的镜像 digest 与本地校验和命令）；带 digest 的 workload
  镜像与平台包 manifest（`pkg-v1-issue127-public-12-299d2216e2b8`，commit `299d2216e2b8`）逐组件一致
  （`build-executor`/`container-executor`/`kubevirt-*` 复用 agent/environment 镜像，`resource-service`
  来自 resource 包，属预期）。补一条更正：早先记的 12 个 workload 含一个非 Deployment 负载，
  以 `-l app.kubernetes.io/instance=labweaver` 的 Deployment 口径为准。
- **本轮部署身份（current，含围栏修复与刷新凭据）**：平台包 `pkg-v1-issue127-public-13-e17f8d966d3d`
  （release `issue127-public-13`，commit `e17f8d966d3d`，8 个组件）；`package-validate` 的 static 与
  connected 均通过，`platform-application` 退出码 0。部署后实机为：`helm status` = `deployed`
  **rev 76**；11/11 Deployment 就绪，`labweaver.io/configuration-bundle-sha256` 取值唯一 =
  `sha256:36e3edba7443df8467b5e2eb6f6373c92a9b2f79ec22d8d6e0c2ed48244d91d2`（即新渲染的
  `configuration-bundle-public-20260924b.yml`，含刷新后的 NATS 凭据）；`agent-service` 镜像 digest
  由 `sha256:0f210313…` 变为 **`sha256:c4d46c121aae3530…`**（含 `normalize_candidate` 围栏修复）。
- **部署后仅追加了格式归一化提交**：在线包 `pkg-v1-issue127-public-13-e17f8d966d3d` 的 `source_commit` 是
  `e17f8d966d3d`；此后为通过 `cargo fmt --all -- --check` 提交了 `d043ada`（3 个文件，逐行核对为 rustfmt
  自身的换行/导入折行，`git diff -w` 对 `main.rs` 为空、对其余两处仅剩 rustfmt 归一化），**语义未变**，
  因此实机镜像与验证结论仍然成立；后续若再打包，注意以新的 `source_commit` 重新记录一次部署身份。
- **A4 复核（当前实机状态）**：`https://keycloak.labweaver.2018wzh.top/realms/workloads/.well-known/openid-configuration`
  的 `issuer`、`authorization_endpoint`、`token_endpoint` 全部是公网主机名；`/auth/login` 返回 307 且
  `Location` 指向公网 Keycloak；`/api/v1/auth/csrf` 在无会话时返回 401（`preflight` 的 portal-csrf 即按此断言）。
  Pod 侧解析走 chart 的 `hostAliases`：`agent-service` 的 Pod 模板把 `keycloak.labweaver.2018wzh.top`
  映射到集群内 identity proxy `10.106.242.177`、`portal.labweaver.2018wzh.top` 映射到公网 Gateway VIP
  `10.99.0.140`，因此 Pod 用公网 issuer 完成 OIDC 发现与令牌交换。
- **修复生效的首个证据**：清掉占用 worker 的陈旧 run（`cancel-stale`，见上文）后，work 旅程的
  agent run（08:58 创建）在**首次尝试**即从 `requested` 走到 `environment:succeeded`（09:26 领取、
  09:30 成功），说明「工具策略 / `--bare` / provider binding / 平台镜像 seed / Harbor 凭据」这几处
  修复合起来已经让 authoring 真正跑通；随后 lab 旅程的 run 立即被领取并进入 authoring。
- **provider binding 修复的端到端验证**：work 旅程随后的环境实例
  （`01a0d2c0-a89c-7fb2-8b8b-7d7475b0e1bb`）带 `provider_binding = container-primary-v1`（即环境服务
  真正注册的那个）并走到 `desired_state=running / observed_state=ready`；与之对照，本轮之前的环境实例
  都是 `kubernetes-work-local-hostpath` + `failed/expiring` + `LW_ENVIRONMENT_PROVIDER_UNAVAILABLE`。
  说明「候选只能使用平台实际注册的 binding」这一改动在真实链路上生效。
- **已修（验收脚本缺陷）**：work 旅程等待资源标题时用 `exact: true` 匹配裸环境 id，而控制台把 Work
  环境命名为 `work-<环境 id>`，因此页面渲染正确也会超时。改为按包含匹配（`.resource-title-row`
  范围内），提交 `4acf1c5`。
- **已知易用性项（有证据，未修）**：失败快照显示环境已 `运行中`、端点表也已出现「健康」，但同一页面
  仍有 `status: 当前正在创建环境，请在操作完成后继续。` 的残留提示；即运行状态与提示文案短暂不一致。
  它不阻断旅程（`assertNoStuckProgress` 只看加载指示器与虚构百分比），作为已知项记录，附截图与本次
  证据（`web/test-results/student-sprint2-flow.live--3dbb6-t-and-releases-its-capacity-student/`）。
- **lab 旅程的双轨迹首次全部成功**：xv6 实验包的 authoring run（09:29 领取、09:38 结束）以
  `succeeded` 收尾，两条轨迹都是 `environment:succeeded, evaluation:succeeded` —— 说明「模型只能
  使用已注册 provider binding」「平台镜像 seed 解析」「`--bare`/工具策略」「沙箱流解析」四处修复
  合起来让 environment 与 evaluation 两侧都能产出被接受的候选（本轮之前两侧都停在候选生成）。
  同一时段 work 旅程的 authoring run 紧接着被领取并进入 `environment:running`，worker 吞吐正常。
- **lab 旅程的学生环境已真正就绪**：实验包发布与审批之后，学生环境实例
  （`01a0d2c7-c42c-7d02-90d7-b53dac0a69a5`）以 `provider_binding=container-primary-v1` 从
  `provisioning` 走到 **`observed_state=ready`**，说明「发布包 → 候选 → 环境实例 → 就绪」整段在
  真实集群上可用；同一时段 work 旅程的 authoring 也在并行推进。
- **Work 模板（environment/evaluation 之外的第三条 authoring 轨迹）仍会拿到 schema 不适配的候选**：
  work 旅程的 run `01a0d2c3-7877-7a10-96b1-acba622c1894` 的沙箱尝试为 `environment|released|exit_code=0`
  （CLI 正常退出），但 worker 侧记录到 `SchemaInvalid` → `LW_EVIDENCE_INVALID`，另有一次
  `ExecutionFailed` → `LW_PROVIDER_UNAVAILABLE`；同一时段 **lab 包的 environment + evaluation 两条轨迹
  双双 succeeded**。即：同一模型在「实验包」两条 schema 上已能产出被接受的候选，而「Work 模板」这条
  仍未通过，属提示/模型能力边界，不是链路或权限问题；work 旅程会重试，失败时按此诊断记录。
- **materials 声明的环境面现在由平台补齐（已修，需重新打包部署 agent-service）**：提示词早已要求
  「逐字保留 materials 的 `terminal`/`entries`/`service_port`」，但本机模型会漏抄（run 53 的 experiment
  候选 `runtime` 段就没有 `terminal`，实验室因此打不开控制台）。现在 `agent-service` 在 **Environment 轨**
  物化候选时，从受验证信封里取出 materials 声明的 `EnvironmentSpec`，把候选**漏掉**的面（`runtime.terminal`、
  `runtime.service_port`、`entries`）按声明值补回：候选显式给了值就以候选为准，运行时变体不同则一概不继承，
  并记 `agent.candidate_materialization.declared_surfaces_restored` 事件。这样 Web 控制台与终端绑定的解析
  不再依赖模型是否照抄，且不引入静默降级——补的是教师已批准素材里的声明值。
- **lab 旅程下一步的真实缺口：experiment 候选没有带 terminal/entry（已定位到「模型漏抄」这一步）**。
  **决定性对照（本轮 run 56 实测）**：示例包 `examples/xv6-lab/environment.yaml` **明确声明**了
  `runtime.terminal: {executable: /bin/sh, args: [], workingDirectory: /workspace}` 与
  `entries: [{name: public-files, protocol: http, servicePort: 8080}]`；而同一包生成的候选所落成的学生环境
  （project `01a0d341-ff64-71e0-b9c2-43083c12e319`）合同里 `entries` 只有那个 http 端点、
  `runtime` 段**没有 `terminal`**。即**素材里有、模型漏抄**——提示词已要求「逐字保留 materials 声明的每个面」，
  27B 本地模型仍会丢字段。后果：控制台能连上（`/connect/console/` 有请求）但 `.xterm-host` 不挂载，
  lab 旅程在终端编辑步骤失败（与 11.8 里同一条目一致）。
  处置（产品决策）：强化提示词/在 schema 层强制容器实验环境必须带 `terminal`，或换满足该要求的上游模型；
  **不得**放宽旅程断言或改写素材来掩盖。
  因果链（run 56，24.4 分钟失败）：候选不带 `terminal` → 控制台虽有 `/connect/console/` 请求但
  `.xterm-host` 不挂载 → 终端编辑（改写 `student/auth.c`）无法完成 → evaluation-service 侧记录
  `LW_COLLECT_SUBMISSION_NOT_FOUND`（收集不到学生提交内容）→ 冻结/结果等待以
  `expect.poll(...).toBe(true)` 的 **300s 超时**告终。因此 lab 旅程的失败是**单一根因**（候选丢 terminal），
  其余现象都是它的下游表现。
  历史佐证（同一现象的早期观测）：spec 已点「打开终端」并等待 `.xterm-host`，页面状态栏显示
  `环境已就绪，可以打开终端；重启会中断当前运行。`，但终端始终未挂载；对应环境实例
  （class `experiment`）的 contract 亦为 `"endpoints": []`/无 terminal。作为归因记录；同批 work 候选
  （`01a0d2cc`）声明了 `http` 端点且观测为 `healthy`。
- **提示词修复已在部署产物中核实**：从运行的 agent-service 镜像
  （`sha256:17e5e5ce…`，`docker create` + `docker cp` 取出 `/usr/local/bin/labweaver-service`）中
  grep 到 `terminal object`（环境提示词新增的“逐字保留 materials 声明的每个面”）与
  `Generate exactly one WorkConfigurationDraft`；`verificationScriptContent` 出现 3 次（提示词与 schema）。
  即 `issue127-public-12` 部署后，两条提示词修复确实在集群里生效，而不是只落在源码里。
- KubeVirt 控制面（virt-api/virt-controller/virt-operator）长期 CrashLoop（报
  `dial tcp 10.96.0.1:443: i/o timeout`），因此 linux-nginx VM+Probe 验收需要先修复 KubeVirt 控制面。
- worker-158 的 P40 驱动与库版本不匹配，需要重载模块或重启节点后才能作为 GPU 提供方。
- **上线阻塞（需产品决定）：项目 LLM 出站策略没有任何应用内建立入口**。证据：
  - 权威读取只认项目行：`services/control-service/src/lib.rs:1963` 的 `active_project_policy`
    是 `WHERE project_id=$1 AND superseded_at IS NULL`，**没有课程级或平台级回退**；
    在线库里 `control.project_llm_policies` 共 198 行，`project_id IS NULL` 的行为 0，即不存在课程策略。
  - 唯一的建立路径是 `POST /api/v1/projects/{id}/llm-egress-policies`
    （`services/control-service/src/api.rs:631`）。前端 SDK 里有 `createProjectLlmPolicy`
    但**没有任何页面调用它**；`web/src/views/teacher/MaterialUploadView.vue` 只读展示
    「项目 AI 设置」（无策略时 `AsyncStateView` 只给重试），`web/src/views/admin/PolicyListView.vue`
    仍是 `PlaceholderPane`（「策略管理」占位）。
  - 项目创建不会自动建立策略（`api.rs:403` 的 `create_project` 只建项目）；文档、`tools/`、`xtask`
    里也没有任何建立策略的运维步骤（全仓 grep `llm-egress-policies` 只命中服务端路由、契约、
    生成 SDK 与 `web/e2e/**` 的验收 spec）。
  - 结论：`web/e2e/**` 的各条旅程用 API 预先建立策略，**替代了一个尚不存在的产品能力**。
    真实教师在一手新项目上会停在「材料上传与 AgentRun」页的错误态，无法启动 AgentRun。
  - 处置（需产品决定，本轮不擅自选定默认策略的 `deniedDataClasses`/`studentContentMode`/预算）：
    ①由平台在项目创建时写入一份审阅过的默认策略（模型/版本/运行时绑定来自部署配置），
    教师只做确认与预算调整；或 ②提供教师/管理员表单（需要先有暴露平台模型与绑定的接口，
    当前 web 无任何平台模型来源）。二者都需要改契约与前端，属产品范围。
- **lab 旅程最后一步的真实阻塞（需产品决定）：学生评测 run 需要资源审批，平台不自动批准**。实测（本轮 run 54）：
  authoring 双轨迹 succeeded、候选 `validated`、构建 succeeded、批准与发布成功（学生环境
  `01a0d330-a006` 达到 `running|ready`），随后学生的评测 run `01a0d331-1f65-74d3-98fb-304eaf1f65f3`
  以 **`LW_EVALUATION_RESOURCE_APPROVAL_TIMEOUT`** 失败（`evaluation.evaluation_runs`，11:33:27；
  同日 2026-09-19 05:26 也有同样一条，属长期行为）。即评测任务会先申请资源（`task` 资源申请，
  与 authoring 沙箱同类），平台**不会**自动批准内部申请，等待超时即判失败。
  两条路径（产品决策，本轮不擅自选定）：
  ①**平台预授权**平台自有的内部任务（authoring 沙箱 + 学生评测）在一份已审阅的额度内自动批准；
  ②**显式审批**：由管理员/课程负责人（或验收旅程）按真实流程批准该申请（`admin` 旅程的
  `POST /api/v1/resource-requests/{id}/approve` 即此路径）。
  验收侧可重复的做法是把 ② 写进 lab 旅程的学生结果阶段（等价于真实部署里管理员批准学生评测），
  但**不得**用 Mock 或放宽断言替代。
- **本轮实测复现：worker 的 claim 循环会静默停住（需 owner 修）**。run 55 的 lab dispatch
  （`run_id=01a0d33b-7f65-7542-810f-f0178ace8163`，创建 11:44:47）在 `agent.agent_run_dispatches`
  里保持 `pending` 超过 2 分钟，而 agent-service（`sha256:c4d46c12…`，本次部署的新镜像）近 20 分钟
  **只打出一条非 HTTP 事件**（`agent.outbox.published`），没有任何 `agent.dispatch.claimed`；
  `kubectl rollout restart deploy/agent-service` 之后立刻出现 `agent.dispatch.worker_started`（×2）与
  **`agent.dispatch.claimed`（×2）**，dispatch 随即被取走。与 §11.8 早先记录的「重启只能让它再 claim
  一次」一致：**claim 循环会在若干次运行后停住且不留日志**，长跑验收会因此空等。处置：需要 agent-service
  owner 给该循环补上「停住即失败/重启」的可观测性与自恢复；验收期间的可重复缓解是发现 dispatch 超过
  ~2 分钟仍为 `pending` 时重启该 Deployment（本轮即如此）。
- **同一卡点的更强证据（run 56）**：重启后 worker 的事件序列恰好是
  `artifact_store.s3_client_ready` → `agent.dispatch.worker_started` → `agent.platform_image.seed_existing`×2 →
  **`agent.dispatch.claimed`**，**之后再无任何事件**（无 `agent.track.started`、无沙箱 Job、无
  `preparation_failed`、无 ERROR 行）；同一 dispatch 在 `agent_run_dispatches` 里于 `prepared` 与 `pending`
  之间来回、`agent_runs.state` 始终 `requested`（`environment:requested,evaluation:requested`）。
  即卡点稳定落在 `claimed` 之后的「绑定/执行」段，且该段**没有可观测输出**——这是本轮验收中
  最需要 owner 处理的产品缺陷；验收侧只能靠周期性重启争取一次成功执行（run 54 的两次 authoring 即
  在重启后正常执行过，说明该路径可用但不可靠）。
- **更正（同轮更晚的实测，重要）**：上面这条「停住」有相当一部分其实是**串行 worker 正被更早的 run 占用**：
  同一时刻 `agent_track_work_items` 里 4 条 `running` 轨迹分别属于 11:39 与 11:44 两个**已被 kill 的验收
  spec 留下的平台 run**，且 `heartbeat_at` 仍在刷新（11:55–11:56），说明它们在正常推进、并非卡死；
  run 56 的 dispatch 只是按 `created_at` 排在它们之后。因此：**杀掉验收进程不会杀掉平台 run**，
  它们会继续占用 worker 直到跑完；新一轮验收会因此排队。排查时应先看 `agent.agent_track_work_items`
  的 `running` 轨迹归属与心跳，再判断是否真的停住；**不要**在 worker 忙碌时重启它（会中断在跑的 run）。
- **每个旅程都有上限，卡死的浏览器不再拖死整轮**：`tools/user_acceptance.py` 以
  `JOURNEY_TIMEOUT_SECONDS=2700`（45 分钟）为单个 Playwright 旅程设上限，超时即终止并以
  `LW_ACCEPTANCE_JOURNEY_TIMEOUT`（`<key>:LW_ACCEPTANCE_JOURNEY_TIMEOUT`）落入证据汇总；此前
  `subprocess.run` 无超时，Playwright 卡在收尾时整轮会无限等待。单测覆盖「超时返回 124」与
  「超时旅程的诊断码」两条。
- **验收证据里的 Playwright 报告改为按旅程写盘（已修）**：平台内置的 HTML/JSON reporter 把配置里的
  相对输出路径解析到**运行目录**上，验收工具原先只从 `web/playwright-report` 拷贝，可能拷到上一次无关
  调用的旧报告。现在每次旅程都用 `PLAYWRIGHT_HTML_REPORT` / `PLAYWRIGHT_JSON_OUTPUT_NAME` 把报告
  **绝对路径**指到 `artifacts/acceptance/<run-id>/<journey>/{index.html,report.json}`，`test-results/`
  仍在旅程结束后拷贝；证据因此自包含，不再依赖仓库里的共享报告目录。
- **lab 旅程的冻结提交等待器写死了另一实验的路径（已修）**：`waitForFrozenSubmission` 在 API 层轮询
  `/frozen-submissions/{id}` 时硬编码要求文件 `student/auth.c`（那是 `real-experiment` 密码实验的路径），
  而 `lab-experiment` 的 xv6 实验冻结的是 `student/student.c`，因此该断言永远不可能满足——run 59 的 lab 段
  在 UI 冻结成功后仍以 `expect(...).toBe(true)` 超时 300s 收场。现在该参数的期望路径由调用方传入
  （`real-experiment` 传 `student/auth.c`，`lab-experiment` 传自己的 `LAB.frozenPath`）。
- **看护的目标判别字段写错了（已修 `30334c3` 之后）**：`/api/v1/resource-requests` 的列表与详情里，
  目标类型在 **`target.kind`**（值 `task`/`environment`），**没有** `targetKind` 这个字段。`561d6a9`
  当初按 `targetKind` 过滤，导致看护把**所有**申请都跳过（`None != 'task'`），平台任务租约因此长期停在
  `reviewing`，authoring 派发拿不到租约而不推进——这正是后续几轮「队列不消化」的直接原因。现在读
  `target.kind`，单测夹具同步改为真实形状（35 passed）。
- **自动审批看护曾与旅程自身的审批抢跑（已修）**：`tools/user_acceptance.py` 的后台看护会批准**所有**
  `reviewing` 申请，包括旅程自己要在管理台手动批准的那一条——run 58 的 work 旅程因此报
  `TimeoutError: locator.fill … 执行后端绑定`（申请已被看护批准，单条审批表单随 `selectedRequest.state`
  变为非 `reviewing` 而消失）。现在看护只批准 `targetKind == 'task'` 的平台任务租约（内部 authoring/evaluation
  任务），`targetKind == 'environment'` 的申请交回旅程自己在管理台批准。
- **审批表单里同名输入框有两个（已修）**：`ResourceApprovalView.vue` 同时渲染批量审批的
  `aria-label="批量审批执行后端绑定"` 与单条的 `aria-label="执行后端绑定"`，因此 `getByLabel(/执行后端绑定/)`
  会命中 2 个元素触发 Playwright strict mode 违规（`REAL_WORK_PRIMARY_FAILURE:locator.fill: strict mode violation`）。
  验收侧改为 `getByRole('textbox', { name: '执行后端绑定', exact: true })`（只命中单条表单那个）。
  同时 `cancelProjectResourceRequestByUi` 的清理可能撞上「渲染后、点击取消前请求已被批准」，平台答 409
  （`LW_RESOURCE_LIFECYCLE_FAILED`）；该 409 属正常的生命周期竞态，现已作为「已被取代」的清理结果返回，
  不再判失败。
- **管理员审批表单的字段标签带后缀，旧断言按 `exact` 匹配会超时（已修）**：`ResourceApprovalView.vue` 的单条审批
  表单里 `<label for="provider-binding">执行后端绑定（CPU 必填）</label>` 与 `aria-label="执行后端绑定"` 并存，
  Playwright 的 `getByLabel('执行后端绑定', { exact: true })` 因关联标签文本带 `（CPU 必填）` 而**匹配不到**，
  work 旅程因此在 3.7 分钟处以 `TimeoutError: locator.fill` 失败（页面快照显示项目
  `live-work-…-01a0d35f`，即已走到资源审批步骤）。验收侧已把匹配放宽为 `getByLabel(/执行后端绑定/)`；
  该字段本身是真实的（CPU 类请求必填），产品无需改动。- **admin 旅程首跑失败于候选构建（也已定位到候选质量）**：run 56 的 admin 用例在 4.0 分钟处以
  `LW_ACCEPTANCE_WORK_TEMPLATE_CANDIDATE_BUILD_FAILED:LW_AGENT_BUILD_PROVIDER_UNAVAILABLE` 失败；build-executor
  的原始事件是 `agent.build_executor.buildkit_solve_failed`（`diagnostic_code=LW_AGENT_BUILD_SOLVE_FAILED`、
  `error_kind=buildkit_solve_rejected`、`failure_stage=build.solve`、`retryable=false`），即 **BuildKit 拒绝了
  该 Work 模板候选的构建配方**（同批另 4 条构建 succeeded，说明构建链路本身正常）。与 lab 的 terminal 缺口同源：
  都是本地 27B 模型产出的候选质量边界，处置属产品决策（提示词/schema 收紧或换模型），不得放宽断言。
- **验收入口现在会代管理员批准平台任务租约（可重复）**：`tools/user_acceptance.py run` 在旅程期间启动一个后台
  审批线程，用 `web/.auth/platform-admin.json` 的会话每 5 秒查一次 `/api/v1/resource-requests`，对仍处于
  `reviewing` 的内部任务租约按管理员流程调用
  `POST /api/v1/resource-requests/{id}/approve`（`expectedRevision` / `providerBinding` / `resources` /
  `durationSeconds` / `reason`，并带 `If-Match` 与 `X-CSRF-Token`），退出前停止。
  这是对「平台不自动批准内部任务申请」这一产品决策点的验收侧对等动作（真实部署里就是管理员在
  `/admin/resource-approval` 点批准）；单测覆盖「只批准 `reviewing`」与「无会话时不动作」两条边界。
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

### 11.9.0 佐证：lab 遗留 run 的终态

killed 旅程留下的 lab run `01a0d2d3-003e-7930-9b9e-d0bf49e14e7b` 最终为 **`partially_succeeded`**，
两条轨迹 `environment:failed, evaluation:succeeded`：evaluation 侧可被接受，environment 侧失败，
与「experiment 候选丢掉了 materials 声明的 terminal/entry（contract 为 `"endpoints": []`）」的诊断
一致；这也说明环境轨迹的失败**不是** provider、凭据或集群问题（同一 run 的另一条轨迹成功落地）。

### 11.9.2 lab environment 轨迹失败的 kind

worker 侧对该轨迹记录的是 `SchemaInvalid` → `LW_EVIDENCE_INVALID`（另有少量 `BudgetExceeded` →
`LW_RESOURCE_EXHAUSTED`）。与环境实例的 contract（`"endpoints": []`）合起来看：候选既没通过 schema，
声明里也缺 `entries`/`terminal`。因此 11.8 里「environment 提示词必须逐字保留 materials 声明的每个面」
这条修复正是对应它的杠杆；确认修复是否生效以重新部署后的轨迹为准。

**更新（已定位并修复其中一个真实缺陷：候选被 ``` 围栏包裹）**。从对象存储读回失败尝试的
`result.json`（`problem-packages/authoring-sandbox/<attempt>/result.json`，取最后一条 `type=result`
的 envelope）可见 CLI 侧是成功的（`subtype=success`、`is_error=false`、`turns` 仅 2），
模型的最终文本是一条**带 ```json 围栏**的 `EnvironmentSpec`：
字段基本齐全（`runtime.kind=container`、`provider_binding=container-primary-v1`、
`terminal.executable=/bin/sh`、`entries[...]`），但 `runtime` 里混用了 `service_port` 这样的 snake_case。
而 `parse_stream_output` 只是把 assistant 的 `text` 块**原样拼接**成候选，
`serde_json::from_str` 遇到围栏必然失败，于是表现为 `SchemaInvalid`。
**已修复**：新增 `normalize_candidate`，在校验前去掉**整体包裹**候选的那一层 Markdown 围栏
（语言标签只允许 `[A-Za-z0-9_+-]*`）；JSON 解析、受保护字段、typed schema、物化等所有闸门仍在围栏内的
文本上运行，因此不可能放进本该被拒的候选；围栏外的散文仍然判 `SchemaInvalid`（不去“搜 JSON”）。
回归测试 `a_fenced_candidate_is_unwrapped_before_validation` 覆盖围栏/无围栏/前后空白/散文包裹四种输入，
`cargo test -p agent-service --lib` 80 用例通过、`clippy -D warnings` 干净。提交 `2b9e726`，
随 `issue127-public-13` 重新打包部署后生效。

### 11.9.1 三条旅程的共同依赖

`lab`、`work`、`admin` 三条旅程都包含「生成 Work 模板 → 候选 → 构建 → 批准」这一段（`admin` 的
`resource-approval.live.spec.mjs` 用同一个 `publishWorkTemplate` 助手建项目与 Work 模板），因此
**Work 模板候选的 schema 适配是三条旅程的共同前置**；`lab` 包另有 environment/evaluation 两条轨迹
（已可双双成功）。修 Work 模板提示词（四个字段必填）后需重新打包部署，三条旅程才能一起转绿。

## 11.9 已知项与未覆盖项

- **未覆盖：GPU / CUDA 旅程**。验收默认只跑 `lab`（xv6）/`work`/`admin` 三条；`LABWEAVER_E2E_LAB=cuda`
  未执行。原因：节点广播的是 `nvidia.com/gpu`（仅 `v1-worker-97`），而 `deploy/versions.lock.yml`
  的 reviewed class 用 `allocation_binding: nvidia-cuda-primary-v1`，且 `resource-service-config/capacity.json`
  未配置 GPU observer（只有 `container-primary-v1`）。条件不满足时不替换为 Mock、也不宣称成功，
  按实记录为未覆盖。
- **未覆盖：`authoring` 旅程的易用性检查点**。`tools/user_acceptance.py` 的旅程表里有 `authoring`
  （教师独立出题），但它属可选旅程，`web/e2e/teacher/authoring.live.spec.mjs` 未接入
  `usability.mjs` 的三个断言；默认三旅程（lab/work/admin）已全部接入。
- **未覆盖：KubeVirt VM 与 linux-nginx 材料链**。virt-api/virt-controller/virt-operator 长期
  CrashLoop（见 §7 与上面的说明），因此 VM 类实验与 Probe 链路未在本轮验收范围内。
- **已知项：环境控制台的状态文案可能短暂滞后**。环境已 `运行中`、端点表已出现「健康」时，页面上
  仍可能残留 `当前正在创建环境，请在操作完成后继续。`；不阻断旅程（`assertNoStuckProgress` 只检查
  加载指示器与虚构百分比），附本次截图证据于 §12 的失败快照目录。
- **待生效（已修，等重新部署）：lab 旅程的 terminal 补齐**。`f66a824` 已在 `agent-service` 里把
  materials 声明的 `terminal`/`entries`/`service_port` 按声明值补回候选（见 §11.8 对应条目与本节上一条），
  但它要**重新打包并部署 agent-service 之后**才对集群生效；在该部署完成前，lab 旅程仍会因候选缺 terminal
  而红。本次已用全新 release 名（`issue127-sandbox-1`）重新打包并校验 manifest 的 `source_commit`
  等于该修复提交，避免与旧同名单据混淆。
- **已知项（需产品决策）：environment 申请的审批窗口短于管理台人工审批的耗时**。run 59 的 work 段三次尝试
  （附 run 60 复核：三次尝试的 `environment` 申请存活 **59.9s / 85.4s / 86.2s**；每次尝试本身持续 2–4 分钟，
  申请**远早于**该次尝试的清理就离开 `reviewing`，因此窗口是平台侧行为，不是旅程清理造成的假象；
  在 resource-service 的 Rust/SQL 里按 `reviewing`+过期/截止条件检索未找到该窗口的定义，触发者应在更上层策略，
  定位留待产品/owner。另据 `services/resource-service/src/lib.rs:464` 的单测，旅程清理发出的取消会落到 `Cancelled` 而不是 `Expired`，因此观测到的 `expired` 不是清理造成的，确实来自平台侧过期。）
  都在管理台审批步骤以 `TimeoutError: page.waitForResponse … POST /api/v1/resource-requests/{id}/approve`
  收场。证据：`resource.resource_requests` 里该旅程创建的 `environment` 申请在创建后约 **86 秒**就由
  `reviewing` 变为 `expiring` 并最终 `expired`（14:05:57→14:07:23、14:09:57→14:11:24、14:14:35→14:16:01
  三次一致）；trace（`artifacts/acceptance/public-20260924-59-all/work/test-results/…-retry2/trace.zip`）
  的网络记录里**只有一次** `resource-requests` 相关的 GET，**没有任何** `/approve` 请求；失败截图显示
  该申请此刻已标为「即将到期」。也就是说：真实浏览器里「进页面→按 key 定位并展开行→填理由/执行后端
  绑定/批准时长→点批准→在确认框点确认」这一串操作来不及在窗口内完成，POST 从未发出。处置属产品侧审批
  窗口策略（放宽 environment 申请的人工审批窗口，或让审批入口更快/支持预填），本轮按证据记录、不放宽断言。
- **已知项：并行验收要避开 worker 串行**。agent worker 一次只处理一个 reserved dispatch，验收前用
  `cancel-stale` 清路、或用 `run` 的默认队列等待；否则旅程会把轮询预算耗在排队上。

### 11.10 本轮重新部署的读回证据（2026-09-24，terminal 补齐上线）

terminal 补齐修复（`f66a824`）随新包上线，实测记录：

- 打包：`cargo xtask package --env v1 --release issue127-sandbox-3 --profile platform --yes`，
  环境变量为 root 侧 `LABWEAVER_KUBECONFIG=/etc/kubernetes/admin.conf`、
  `LABWEAVER_PLATFORM_REGISTRY=harbor.lab.lan`、
  **`LABWEAVER_BUILD_PROXY=http://49.52.27.95:7897`**（漏传会命中 `agent-service` 镜像里
  `npm pack @anthropic-ai/claude-code-linux-x64` 的构建步骤失败）、`RUSTC_WRAPPER=`（避免 root
  下 sccache 连不上自己的 server）。产物
  `artifacts/package/pkg-v1-issue127-sandbox-3-79173741bcf9/PlatformImagePackageManifest.json`，
  `source_commit=79173741bcf9…`（`git merge-base --is-ancestor f66a824… <commit>` 为真）。
- 校验：`package-validate --mode static` 与 `--mode connected --env v1` 均 **exit 0**。connected
  会先 `docker buildx inspect --bootstrap` 核对 BuildKit 身份，因此本地 `docker buildx use` 选中的
  builder 必须指向**真实在跑的**端口转发（本轮把失联的 `bk-local`（1236）换成指向 1234 的 `bk-local2`
  后才通过）。
- 部署：`cargo xtask platform-application --env v1 --infra --yes --package-manifest <新包>`，
  复用 `application-vars-public-20260924.yml`；**exit 0**，helm `labweaver` 升到 revision 78 / deployed。
- 读回：11 个 workload 的镜像 digest 与 manifest **逐项 MATCH**（`agent-service`/`build-executor`
  由 `c4d46c12…` 换为 `3b8f99a3634b…`，`environment-service` 及其执行器为 `dc2e4ccd…`，
  `web` 为 `e56fef48…`，`resource-service` 属 resource 包未变）；部署后 `preflight` exit 0。

### 11.11 三条旅程的本轮结论（run 59，2026-09-24）

run 59（`public-20260924-59-all`，`lab,work,admin`，公网域名、真实 Keycloak、真实模型）三段均为
failed，逐段定性如下（每段证据都在 `artifacts/acceptance/public-20260924-59-all/<段>/`）：

| 段 | 结果 | 根因 | 处置 |
|---|---|---|---|
| `lab` | failed（run 59 12.1m；run 60 同因） | 共享等待器把冻结提交的必需文件写死为 `student/auth.c`（属另一实验），xv6 实验冻结的是 `student/student.c`，断言永不满足 | **已修** `fc3a8ce`（路径由调用方传入，并与 `examples/xv6-lab/manifest.json` 交叉核对） |
| `work` | failed（1.9m/3.9m/…） | 管理台人工审批来不及在 environment 申请的审批窗口内完成（实测窗口≈86s，trace 中无 `/approve`） | **留证，待产品决策**（§11.9 末条） |
| `admin` | failed（provider 抖动，已加固） | Work 模板 authoring run 撞上模型服务的瞬时 provider 不可用（`LW_PROVIDER_UNAVAILABLE`，`duration_ms: 0`） | **已加固** `589dce0`（仅对该诊断重试一次，其它失败原样上报） |

run 60（`lab,work`）复跑后两段的失败点也各前进了一步：`lab` 已越过冻结提交那段，改为 authoring run
以 `partially_succeeded:LW_PROVIDER_UNAVAILABLE` 结束（模型服务瞬时抖动，`fc3a8ce` 的冻结路径修复本身已
生效）；`work` 仍卡在审批填写（`执行后端绑定` 定位超时，说明申请已离开 `reviewing`），与上表一致。

- **admin 旅程的候选构建同样受 provider 抖动影响（已加固）**：run 61 的 admin 段先越过 authoring（provider 重试生效），
  随后的**候选构建**以 `LW_AGENT_BUILD_PROVIDER_UNAVAILABLE` 失败——同一类瞬时抖动发生在平台紧随其后的构建步骤上。
  现在构建失败也纳入同一重试循环（仅限该诊断，重试时等「启动 Work AgentRun」重新可用），其它失败原样上报。
另有两处更早定位并修复的旅程缺陷：审批表单同名输入框的定位歧义（`7ea6ba6`）、后台自动审批看护与
旅程自身审批的抢跑（`561d6a9`）。修复后由 run 60（`lab,work`）与后续 run 验证；`environment` 申请
的审批窗口竞态未在任何环节放宽断言。

### 11.12 lab 旅程的新阻塞：OJ 评测沙箱（run 61，2026-09-24）

run 61 的 lab 段已越过 authoring（provider 重试生效）与冻结提交，停在评测结果：
`REAL_EXPERIMENT_EVALUATION_FAILED:failed`。证据链：

- `evaluation.evaluation_runs` 最近四次均为 `failed` / `diagnostic_code=LW_OJ_SANDBOX_UNAVAILABLE` / `awarded_score=0`；
- `evaluation.evaluation_step_attempts` 对**同一 runner 镜像**出现 `failed(LW_OJ_SANDBOX_UNAVAILABLE)`
  与 `succeeded` 交替，说明 OJ 执行链路本身可用，失败是**部分步骤**；
- `labweaver-evaluation` 里对应 Job（`lw-oj-01a0d40b…`）先是容器正常 Start（镜像来自 Harbor 的
  项目实验镜像），随后以 `BackoffLimitExceeded` 结束——即容器**反复非零退出**；
- 平台把「重试耗尽」统一映射为 `LW_OJ_SANDBOX_UNAVAILABLE`。

独立探针已排除「镜像+gVisor 不可用」这一可能：用**同一个 runner 镜像**、`runtimeClassName: labweaver-sandbox`、
符合 `labweaver-evaluation` 命名空间 `PodSecurity: restricted` 的 securityContext 提交探针 Job，**成功**执行
（`/proc/version` = `Linux version 4.19.0-gvisor`，uid 1000，根文件系统正常）。因此失败落在
`evaluation.yaml` 的 `compile` 门（`runner.kind=program`、`toolchainProfile: xv6-riscv64`、`phase: compile`、
`input: student/student.c`）本身：编译阶段返回非零。注意该任务一开始把**所有非零退出**都映射成
`LW_OJ_SANDBOX_UNAVAILABLE`，所以待查项是：OJ 容器在这份 xv6 实验镜像与 gVisor 运行时下为何非零退出，以及「测试未通过」与
「沙箱不可用」是否被混为同一诊断码（后者会让用户看到误导性的不可用提示）。属执行/评测侧 owner 决策。

### 11.13 admin 段的候选构建：BuildKit 拒绝 solve（run 62，2026-09-24）

run 62 的 admin 段两次尝试都在候选构建处以
`LW_ACCEPTANCE_WORK_TEMPLATE_CANDIDATE_BUILD_FAILED:LW_AGENT_BUILD_PROVIDER_UNAVAILABLE` 结束。
build-executor 日志（近 30 分钟）显示 12 次 `agent.build_executor.buildkit_solve_failed`，
`error_kind=buildkit_solve_rejected`、`diagnostic_code=LW_AGENT_BUILD_SOLVE_FAILED`——即 **Work 模板候选的
构建配方本身不可解**；旅程侧的 provider 重试（`979ed79`）确实执行过（run 62 用的就是带重试的 spec），
两次都得到同一结果。这与 lab 的 terminal 缺口同源：本地 27B 模型产出的候选质量边界，处置属产品决策
（收紧提示词/schema 或换模型），不放宽断言。证据：`artifacts/acceptance/public-20260924-62-all/admin/`。

### 11.19 OJ 运行时修复的端到端验证（run 68，2026-09-24）

修复部署后第一个真实 OJ 执行（evaluation run `01a0d49b-b8a0…`，18:09:31）得到**决定性证据**：

- 抢到的 OJ Pod `lw-oj-01a0d49bb9e17f519916-sd47f`：**`runtimeClassName` 为空（默认 runc）**，
  `program-runner` 容器状态 **`Completed`**——此前同一镜像下它是 `Error` + `OjWorker(SandboxUnavailable)`；
- 评测结果随之从 `LW_OJ_SANDBOX_UNAVAILABLE` 变为 **`LW_OJ_COMPILE_ERROR`**（`compile` 门 `failed`，
  `smoke-tests` 因依赖 `skipped`），即**平台层缺陷已消除**：OJ 现在能正常建立陆锁沙箱、跑完容器，
  并把「编译失败」作为真实诊断报出来（而不是误报为沙箱不可用）。

**编译失败的排查线索**：冻结提交 `01a0d49b-b0ab…` 的内容与
`examples/xv6-lab/student/student.c` **完全一致（同为 114 字节）**，而独立探针证明该文件在同一 runner 镜像里
`build-xv6.sh` 可 `BUILD_EXIT=0`。因此差异不在源码。已排除「素材包缺 supportFiles」这一可能：`examples/xv6-lab/manifest.json` 共 96 个文件，
`xv6/xv6-source.tar.gz`（153 KB）与 `scripts/*` 都在其中，磁盘上也在。平台侧调用与探针的差异因此只剩
**运行资源与路径契约**：runner 容器的限制是 `cpu: "1"`、`memory = 请求值 + 256MiB`（下限 512MiB）、
`ephemeral-storage: 256Mi`，`build` 卷 `sizeLimit: 128Mi`，而 profile 用 `{evaluator_dir}`（= `/support`）。
下一步应核对 `/support` 的落盘结果与 `compile` 的容器输出——后者目前仍未落盘（§11.14 的建议只完成了
「换对诊断码」这一半）。

**仍未覆盖**：失败切换成了编译本身——而独立探针证明 starter（`examples/xv6-lab/student/student.c`）
在同一 runner 镜像里 `build-xv6.sh` 是 `BUILD_EXIT=0`。因此下一步是查**冻结后的提交内容**为何编译不过
（实验材料/冻结语义），另外 `compile` 的容器输出目前仍未落入平台日志或尝试记录（§11.14 的建议只完成了
「换对诊断码」这一半）。

### 11.15 OJ `LW_OJ_SANDBOX_UNAVAILABLE` 的根因：Landlock 与 gVisor 不兼容（已修，待部署验证）

**根因（已证实）**：评测服务的 OJ 程序沙箱用 **Landlock**（`services/evaluation-service/src/oj_worker.rs`
的 `apply_compiler_filesystem_sandbox`：`CompatLevel::HardRequirement`，要求 `RulesetStatus::FullyEnforced`
且 `no_new_privs`，否则返回 `SandboxUnavailable`）。而 develop 把一次性工作负载统一到了 gVisor 的
`labweaver-sandbox` RuntimeClass，**gVisor 不实现 Landlock 系统调用**。

**直接证据（同镜像、同 securityContext、只换 RuntimeClass 的对照探针）**：

| RuntimeClass | `landlock_create_ruleset` 结果 |
|---|---|
| `labweaver-sandbox`（gVisor） | `-1` / `errno=38`（`Function not implemented`，ENOSYS） |
| 默认（runc） | 进入真实内核（返回 `EFAULT`，说明系统调用存在） |

同一探针也证明镜像里的 xv6 编译链路本身可用（`build-xv6.sh` 以正确契约跑出 `BUILD_EXIT=0`），
即失败不在镜像、工具链或源码。

**运行期直接证据**：在清理之前抢到的 OJ Pod 容器状态与日志为
`init:materialize-input=Completed`、`main:program-runner=Error`，`program-runner` 的输出是
**`Error: OjWorker(SandboxUnavailable)`**——即取件（init）已成功，失败精确落在 worker 自己的
Landlock 步骤上，与代码位置和对照组探针结论一致。

**修复**：`oj_job.rs` 不再给 OJ Job 设置 `runtimeClassName`（改用节点默认运行时），OJ 的隔离由它
**自己**的 Landlock 规则集提供，而 authoring/probe 等仍留在 gVisor。测试
`services/evaluation-service/tests/oj_job.rs` 相应改为断言该字段为空，并注明原因。

**部署与读回**：`cargo xtask package --env v1 --release issue127-oj-1 --profile platform --yes`
（root、带 `LABWEAVER_BUILD_PROXY` 与 `RUSTC_WRAPPER=`）产出
`artifacts/package/pkg-v1-issue127-oj-1-a895dcb9d7a0/`，`source_commit=a895dcb9d7a0…`（即该修复提交）；
`package-validate` 的 `static` 与 `connected --env v1` 均 **exit 0**；重新施加后 helm `labweaver` 到
**revision 80 / deployed**，读回 `evaluation-service` 的 digest 与 manifest **MATCH**
（`sha256:a9a149dfe902f9b…`）。随后以 run 65 复跑 lab 段做端到端验证。

**注意**：这属于**隔离边界**的选择，涉及 `AGENTS.md` 中「核心权限与隔离由核心负责人评审」的约定，
需 owner 复核；本轮按「当前配置下 OJ 完全无法执行」的事实修复，并在修复后重新打包部署、复跑 lab 旅程验证。

### 11.14e run 70（验证收据修复的那一轮）的实际阻塞：模型候选质量

收据修复部署后（helm rev 84）跑 run 70，其 authoring 出现 **`environment` 轨 failed、`evaluation` 轨
succeeded**（run 终态 `partially_succeeded`），近期 agent-service 诊断只有 `LW_PROVIDER_UNAVAILABLE`×2
与 **`LW_EVIDENCE_INVALID`×2**——即**候选没过受审 schema**，属模型/提示词质量边界（与 §11.13 的
「构建配方被 BuildKit 拒绝」同类）。因此这一轮**没有走到 OJ**，`evaluation.oj.compile_failed` 事件
（携带编译退出码）仍处于「已部署、待触发」状态：下次有 run 真正进入 OJ 时即可见。

### 11.14d 为什么编译输出目前无法进入平台记录（架构性结论）

尝试在证据里携带编译输出时确认了平台的既有设计：`OjEvidenceReceipt` 被明确注释为
**payload-free**（只含身份、摘要、`evidence_sha256`/`evidence_size_bytes`、终态与分数），它经容器的
**termination message** 回传给 evaluation-service；真正的载荷写在 Job 自己的 `/evidence/evidence.json`
（`EVIDENCE_PATH`），而该卷**没有被平台回收**——`/evidence` 是 `emptyDir`，随 Pod 消失。
所以「把编译输出写进 evidence」在当前架构下**不会**到达平台记录，而把 payload 塞进 receipt 又与该
「payload-free receipt + 有界 termination message」的设计相悖。

**已实现的最小方案（`4a17a1d`）**：把**编译退出码**放进 payload-free receipt（`compile_exit_code`），
并在 evaluation-service 侧于失败时打 `evaluation.oj.compile_failed` 事件（含 exit code 与诊断码）。
一个整数不破坏 receipt 的精简约定，却足以区分失败类别——`build-xv6.sh` 自己的早退是 64/65/66，
其它码就是 `make` 的真实失败。载荷（完整 stdout/stderr）仍留在 §11.14 的容器日志路径上。

**若要拿到完整输出，仍需在执行侧做一件事**（任一即可）：把失败 Job 保留一段时间（或失败时不删）以便读容器日志；
或由 evaluation-service 在删除前读取容器日志/回收 `/evidence` 卷；或允许 receipt 携带**有界**诊断尾巴。
本轮已把 §11.14/§11.14b/§11.14c 的现场证据与这条架构结论一并留给执行侧 owner。

### 11.14c 编译失败的耗时只有约 2 秒（新证据，指向脚本早退）

run 69 期间从 `labweaver-evaluation` 的事件里读到 OJ Job 的完整生命周期：
`Pulled`(3m22s) → `Created` → `Container started`(3m21s) → **`Completed  job/lw-oj-…`(3m19s)**，
即容器**从启动到结束只有约 2 秒**；而对照探针里同一 runner 镜像跑 `build-xv6.sh` 需要 **60–90 秒**
（解包 + `make kernel/kernel fs.img user/_student`）。因此这**不是**编译耗时或超时（`wallTimeSeconds: 30`
也没触发），而是脚本在**极早期就退出**——最可能是 `build-xv6.sh` 的前置校验分支：`exit 64`（参数个数）、
`exit 65`（source/binary 路径契约）、`exit 66`（`$evaluator_dir/xv6/xv6-source.tar.gz` 缺失，或
`$build_dir/xv6` 已存在）。平台侧路径来自 `OJ_SUBMISSION_ROOT=/input/submission`、
`PROGRAM_BINARY_PATH=/work/build/program`、`SUPPORT_ROOT=/support`，与契约一致；素材包也确认含该 tar
（153 KB）。**镜像与 profile 已核对无误**（就地探针读取当次 OJ 的 runner 镜像）：`/opt/labweaver/profiles/xv6-riscv64.json`
的 `compileArgv`/`runArgv` 与示例一致，`supportFiles` 正是
`scripts/{build,run}-xv6.sh`、`scripts/run-xv6.py`、`xv6/xv6-source.tar.gz`；`/opt/labweaver/xv6/xv6-source.tar.gz`
**存在且为 153385 字节**，`/opt/labweaver/scripts/*` 四个脚本齐备。因此「素材缺失」这一支也被排除。

**下一步应当直接看 `/support` 的落盘结果与容器 stderr**——这也再次说明「失败即删 Pod」
（§11.14b）是当前唯一的取证障碍。

，捕获窗口仍然极短

`6b98326` 让编译失败把有界的 stdout/stderr 打进 `program-runner` 的容器日志。实测（run 69，
evaluation run `01a0d4b7-d7d5…`，18:40:14）：OJ Pod 从创建到被平台清理**不到 50 秒**
（`Init:0/1` → 运行 → 消失），因此「等容器写出日志再 `kubectl logs`」是一个**竞态**：
1 秒轮询的观察器与直连检查都没能在窗口内取到 `oj compile` 输出，Pod 已 NotFound。

**结论**：要让编译失败可回溯，平台侧需要**留下证据**而不是依赖运维抢时间——例如把失败 Job 的 Pod
保留一段（`ttlSecondsAfterFinished` 或失败时不删），或由 evaluation-service 在 Job 结束前读取容器日志
并写入尝试记录/事件。本轮已把「打印输出」这一半做完（§11.14），剩下这一半属执行侧 owner 决策。

（**已部分修复**：编译输出现在进容器日志）

**更新（`6b98326`）**：`persist_evidence` 只把 stdout/stderr 的**哈希与字节数**写进 `/evidence/evidence.json`，
而该文件位于 Job 自己的卷里、随清理消失——所以编译失败曾**完全没有可回溯的输出**。现在编译失败时会把
**有界的编译器 stdout/stderr（8 KiB，标注是否截断）**镜像到 `program-runner` 的容器日志，平台与
`kubectl logs` 都能读到。诊断码的混淆也已随之消解（见 §11.19：失败从
`LW_OJ_SANDBOX_UNAVAILABLE` 变为语义正确的 `LW_OJ_COMPILE_ERROR`）。

（run 61/63 观察，2026-09-24）

复核确认：**失败之后 OJ 的 Job/Pod 会立即消失**（`labweaver-evaluation` 里只剩历史探针 Job，`lw-oj-*`
查不到、`kubectl logs` 无从取），平台也没有把容器 stderr 记进任何事件或 `evaluation_step_attempts`
（该表只有 `diagnostic_code`）。因此 `LW_OJ_SANDBOX_UNAVAILABLE` 这类失败**没有任何可回溯的原始输出**，
只能靠外部探针反推。

另有一项更强的复现结论：把示例里的 starter（`examples/xv6-lab/student/student.c`）按 profile 的
`compileArgv` 契约（source 在 submission 目录内、binary 在 `/work/build/` 下）喂给**同一个 runner 镜像**，
在 `labweaver-sandbox` 下 `build-xv6.sh` **成功**（`BUILD_EXIT=0`，完整 make 日志显示 kernel、fs.img 与
`user/_student` 均构建完成）。因此镜像、工具链、vendored xv6 源码与 starter 本身**都没有问题**，
失败落在平台对 OJ 步骤的编排/取材环节。同时确认**运行环境本身完好**：用同一 runner 镜像 + `labweaver-sandbox` 起探针 Job，镜像里
`/opt/labweaver/{scripts,profiles,xv6,hidden-tests}` 齐备（`build-xv6.sh`、`run-xv6.sh`、
`profiles/xv6-riscv64.json`、`hidden-tests/xv6-riscv64/{smoke,filesystem}.{in,out}` 均在），探针可正常执行。
即：问题不在镜像与沙箱，而在 compile 阶段本身**且其输出不可追溯**。

按「保留正常诊断上下文、不能让故障无法诊断」的要求，建议执行/评测侧补齐：Job 失败时保留最后一次
容器日志（事件或尝试记录），并把「测试未通过」与「沙箱不可用」分开诊断码。

### 11.16 重新施加部署后 agent 派发循环会停摆（运维要点）

本轮多次观察到：`cargo xtask platform-application` 重新施加后，`agent-service` 的派发循环有时**不再认领
新的 AgentRun**——`agent.agent_runs` 里 run 长时间停在 `requested`（`environment:requested`），
`agent-service` 只在正常应答 HTTP（前端轮询 200），日志里既没有 `agent.dispatch.worker_started` 也没有
`agent.dispatch.claimed`，authoring 命名空间没有任何 Pod。

**根因补充（后经证实）**：真正让队列不消化的不是派发循环本身，而是**看护的判别字段写错**
（`target.kind` 被当成不存在的 `targetKind`，见 §11.18 上方与 `a0f7130`）与**容量同步把 task 租约喂给
只处理 environment 的提供者**（`30334c3`）。两处修好并部署后，租约能获批、派发随即恢复；重启
`agent-service` 只是当时的临时缓解。

**处置（实测有效）**：`kubectl -n labweaver-system rollout restart deploy/agent-service`。重启后立刻出现
`agent.dispatch.worker_started` 与 `agent.dispatch.claimed`，队列随即开始消化。因此遇到「旅程长时间卡在
authoring、且集群里没有 authoring Pod」时，先确认派发循环是否在跑，再做重启，不要把它误判成产品缺陷。

### 11.18 清理「认领后卡住」的旧 AgentRun（运维要点）

楔死期间留下的 `requested` run 会让验收入口的队列等待一直不收敛，需要人工清理。两条实测细节：

- **取消 AgentRun**：`POST /api/v1/projects/{projectId}/agent-runs/{runId}/cancel`，必须带 **`If-Match`**
  （用详情响应的 `ETag`；只带 body 里的 `expectedRevision` 会得到 `412 lw_urn:if_match_required`）。
  用 **teacher 会话**（项目所有者）即可，`admin` 对他人项目会 `LW_AUTH_SCOPE_DENIED`。成功返回 `202`。
- **清理孤儿租约**：`POST /api/v1/resource-leases/{id}/revoke`，body 需 `expectedRevision`
  （缺字段是 422），同样用有 scope 的会话。
- 队列不收敛时也可给验收入口加 `--no-queue-wait` 直接开跑（该 flag 语义是「不等排队的 authoring 派发」）。

### 11.17 孤儿 task 租约会永久拖住容量同步（本轮阻塞验收的集群状态问题）

**现象**：run 61 之后集群的 authoring 队列不再消化——`agent.agent_runs` 多条停在 `requested`，
`labweaver` 各命名空间没有 authoring Pod，`agent-service` 只在应答前端轮询；`resource-service` 日志持续
以每秒数条刷 `resource.lease.sync_failed` + `diagnostic_code=LW_RESOURCE_TASK_OWNER_REQUIRED`。

**证据**：日志里失败集中在两条**昨天遗留**的租约上（`01a0d012-e369-…` 与 `01a0d008-bdeb-…`，各自
每分钟上百次），它们的 `task_run_id` 早已不存在（`resource_requests` 近 30 分钟无新行、也不再有
`targetKind=task` 的新申请），而租约状态长期停在 `active`。用平台自己的 API 撤销后两租约进入
`expiring`，但同步仍失败、计数不降（重启 `resource-service` 亦然）。

**根因（已定位并修复）**：容量同步的取件 SQL `next_unsynced_active_lease` 只按
`c.state='handed_off' AND l.state='active' AND c.lease_synced_revision < l.revision` 选租约，**没有**
按 `r.target_kind` 过滤；而同文件的 `next_lease_cleanup` 等路径都带 `r.target_kind='environment'`。于是
**task 目标的租约被送进了只处理 Environment 的容量提供者**，在 `capacity.rs` 的
`let ResourceTarget::Environment {..} = .. else { TaskOwnerRequired }` 处必然报错，并且**永久重试**。
修复：给该取件 SQL 补上 `r.target_kind='environment'`（与既有过滤一致）。`cargo test -p resource-service`
的 17+4+22 个用例全过（含直接覆盖该取件的 postgres 用例）。

**上线与实测效果**：`cargo xtask package --env v1 --release issue127-res-1 --profile resource` 产出
`artifacts/package/pkg-v1-issue127-res-1-30334c3a4b83/`（`source_commit=30334c3a…`，单组件 `resource-service`），
`package-validate` 的 `static`/`connected` 均通过。注意 **`resource-application` 角色读的是另一套环境变量**
（`LABWEAVER_RESOURCE_CONFIGURATION_BUNDLE` / `LABWEAVER_RESOURCE_VALUES_FILE` /
`LABWEAVER_ACCESS_SEED_FILE` / `LABWEAVER_POSTGRES_SERVICE(_FILE)`，见
`deploy/ansible/roles/resource_application/defaults/main.yml`），与平台档的 `LABWEAVER_APPLICATION_VARS_FILE`
不是一回事；本轮因未备该套变量，改用 `kubectl -n labweaver-system set image deploy/resource-service
resource-service=<新包 digest>` 上线（rollout 成功）。**实测效果**：上线后 2 分钟内
`LW_RESOURCE_TASK_OWNER_REQUIRED` 由约 190 条/2 分钟降为 **0 条**，`succeeded` 的 run 数上升，楔死解除。

**与派发停摆的关系**：重启 `agent-service` 后只有 `agent.dispatch.worker_started` 与**一次**
`agent.dispatch.claimed`（run `01a0d432-ffeb…`），此后再无任何派发事件、该 run 也没有任何 track 启动——
即 §11.9 里曾记录过的「认领后不推进」形态，且它正好发生在容量模块被孤儿租约拖住期间，两者表现一致。

**进一步确认**：两租约**连同它们的 request** 都已被平台推进到 `expiring`（`resource_requests.state=expiring`、
`revision=4`），对该 request 再发 `cancel` 得到 409 `LW_RESOURCE_LIFECYCLE_FAILED`——即状态机已经走完它
能走的部分，卡点在「release/同步需要 task owner」这一步。重启 `agent-service` 后派发只认领了一次
（`agent.dispatch.claimed`），随后再次停摆，authoring 命名空间始终无任何 Pod。

**判断**：这是资源域的一个健壮性缺口——**task 租约的 task owner 消失后，租约不会自行终结，容量同步
因此永久失败**，并可阻塞后续派发。为验收放行我做了两件都在平台能力内的事：`rollout restart`
`agent-service`（见 §11.16，可短暂恢复派发）与用 **teacher 会话**（admin 会话对该租约是
`LW_AUTH_SCOPE_DENIED`）按契约调用 `POST /api/v1/resource-leases/{id}/revoke` + `expectedRevision`
（返回 200）。修复「owner 消失即终结租约」属资源域 owner 决策，本轮按证据记录，不改数据库。

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

**排队等待（重要）**：agent worker 一次只处理一个 reserved dispatch（`created_at` 先到先处理），
单次运行实测 10-40 分钟，因此 `run` 默认先等待队列排空再启动旅程（每 30 秒查一次
`agent_run_dispatches` 的 `pending/preparing/claimed`，上限 1 小时，深度变化时打印一行）。需要
立刻开始时用 `--no-queue-wait` 跳过；`preflight` 的 `authoring_queue` 检查始终报告当前深度
（非零时带 `LW_ACCEPTANCE_AUTHORING_QUEUE_BUSY`，仅告警不阻断）。旅程自身的轮询上限已按
「数条排队运行 + 部署 15 分钟 LLM 界 + 镜像构建」放大（`FULL_CHAIN_TIMEOUT_MS` 4h、
`AUTHORING_RUN_TIMEOUT_MS` 2.5h、`CANDIDATE_BUILD_TIMEOUT_MS` 1h）。

**清理陈旧 run（已实现为子命令，并实测）**：`tools/user_acceptance.py cancel-stale --base-url <url>
[--auth-dir <dir>] [--keep <run-id 前缀>]` 会列出所有非终态 agent run，用 owner 会话逐个取消
（`--keep` 保护正在验收的那一条），输出每条的 `http-<状态码>`。它要求 `<auth-dir>`（默认 `<repo>/.auth`）
里已有该角色的 Playwright 会话，可先执行下面的 `--project=setup` 生成；手工等价步骤与稳定诊断码如下：

**手工步骤（已实测）**：worker 按 `created_at` 串行处理，被跳过的旅程会留下占用
worker 的运行，因此验收前应把它们取消。取消接口是用户态的，实测步骤如下：

```sh
# 1) 取得对应角色（通常是 project owner）的新会话；会写 <repo>/.auth/<role>.json
LABWEAVER_BASE_URL=https://portal.labweaver.2018wzh.top LABWEAVER_IGNORE_HTTPS_ERRORS=1   node web/node_modules/@playwright/test/cli.js test --config=web/playwright.config.mjs --project=setup
# 2) GET /api/v1/auth/csrf 取 X-CSRF-Token；3) GET /api/v1/projects/<p>/agent-runs/<r> 取 ETag（形如 "rev-1"）
# 4) POST /api/v1/projects/<p>/agent-runs/<r>/cancel
#    headers: X-CSRF-Token、If-Match: "rev-N"、Idempotency-Key: <uuid>
#    body:   {"reason": "..."}          -> 202 接受
```

实测要点：`platform-admin` 对项目范围内的 run 会得到 `lw_auth_scope_denied`（403，属正确的权限行为），
必须用 **owner**（teacher/student）会话；缺 `If-Match` 为 `lw_if_match_required`（412），缺
`Idempotency-Key` 为 `lw_idempotency_key_required`（400）；取消是异步的，接口返回 202 后队列会随之缩短。

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
