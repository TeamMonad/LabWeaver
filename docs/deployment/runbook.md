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

## 6. OJ 评测运行时

OJ Job 使用 `runtimeClassName: labweaver-oj`。该运行时由 `deploy/ansible/roles/oj_runtime` 安装，在 `site.yml` 中于 KubeVirt 之后、addons 之前对 `k8s_cluster` 执行。

当前树**没有**单独运行该角色的 playbook 或 xtask 入口；支持的入口是完整 `site.yml`。在既有集群上补装时需重跑 `site.yml`，或由运维以临时 play 调用该角色。

角色完成并回读：

- 每台 worker 安装 CRI-O drop-in `/etc/crio/crio.conf.d/99-labweaver-oj.conf`：
  ```ini
  [crio.runtime.runtimes.labweaver-oj]
  runtime_path = "/usr/local/libexec/labweaver-oj-runtime"
  runtime_type = "oci"
  ```
- wrapper `/usr/local/libexec/labweaver-oj-runtime` 只对 `create`/`run` 改写 OCI `config.json` 的 `linux.resources.pids.limit`（默认 128），随后 `execv /usr/bin/runc`；其他命令原样透传。
- 要求 CRI-O `cgroup_manager = "systemd"` 且 `crio` active。
- 控制平面创建 `node.k8s.io/v1` `RuntimeClass/labweaver-oj`，handler 为 `labweaver-oj`。

逐节点校验：

```sh
cat /etc/crio/crio.conf.d/99-labweaver-oj.conf
crio config | grep -A2 'labweaver-oj'
systemctl is-active crio
kubectl get runtimeclass labweaver-oj -o jsonpath='{.handler}'
```

最小 OJ Job 验证（一次运行、用完即删；显式设置非 root 安全上下文以匹配运行时约束）：

```sh
kubectl create namespace oj-runtime-probe
kubectl apply -f - <<'YAML'
apiVersion: batch/v1
kind: Job
metadata:
  name: oj-runtime-probe
  namespace: oj-runtime-probe
spec:
  backoffLimit: 0
  ttlSecondsAfterFinished: 300
  template:
    spec:
      runtimeClassName: labweaver-oj
      automountServiceAccountToken: false
      restartPolicy: Never
      containers:
        - name: probe
          image: docker.io/library/busybox@sha256:73aaf090f3d85aa34ee199857f03fa3a95c8ede2ffd4cc2cdb5b94e566b11662
          command: ["sh", "-c", "cat /sys/fs/cgroup/pids.max; cat /sys/fs/cgroup/pids.current"]
          securityContext:
            runAsNonRoot: true
            runAsUser: 65532
            allowPrivilegeEscalation: false
            readOnlyRootFilesystem: true
            seccompProfile: { type: RuntimeDefault }
            capabilities: { drop: ["ALL"] }
YAML
kubectl -n oj-runtime-probe wait --for=condition=Complete job/oj-runtime-probe --timeout=120s
kubectl -n oj-runtime-probe logs job/oj-runtime-probe   # pids.max 必须为有限值且 <= 128
kubectl delete namespace oj-runtime-probe --wait=true
```

handler 不可用或 PID 上限不生效时，OJ 调度必须失败关闭；不要为让运行时“可用”而放宽 seccomp、Landlock 或 no-new-privileges。

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
