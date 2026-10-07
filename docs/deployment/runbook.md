# LabWeaver 部署与运维手册

本手册描述当前仓库对应的 v1 集群入口。它覆盖首次安装、应用增量部署、配置恢复、GPU 节点维护、故障排查和用户操作。命令名以 `xtask/src/main.rs`、`deploy/ansible/playbooks/` 和对应 Ansible role 为准；私有清单、证书、令牌、口令和用户内容只放在受保护的私有目录，不复制到仓库或普通日志。

应用层由 Control、Access、Environment、Agent、Evaluation、Resource、Web 和 OpenSSH Gateway 组成。Environment 管环境对象，Resource 管申请、租约、容量和费用，Evaluation 管评测任务；部署操作不能绕过这些业务边界。

> 应用 Helm 升级使用 `--wait` 和配置的超时，并保留失败后的 release 状态供运维修复和重新 apply；升级不自动回退。迁移是前向操作，应用回退不能恢复数据库。只有操作者明确选择与当前数据库契约相容的版本时，才允许手动 `helm rollback`。删除数据库、共享存储、基础服务或用户环境前，必须先确认对象归属、数据保留和变更窗口；本手册不提供批量删除或节点重启来制造故障的步骤。

## 1. 前提与私有输入

### 1.1 控制器和工具

在批准的 Linux 控制器上从仓库根目录执行基础设施命令。Rust、Helm、kubectl、Ansible、BuildKit 和 `virtctl` 的版本以 `deploy/versions.lock.yml`、`deploy/ansible/controller.lock.yml` 为准。需要手工运行 Ansible 时先安装仓库声明的 collection：

```sh
ansible-galaxy collection install \
  -p deploy/ansible/collections \
  -r deploy/ansible/requirements.yml
```

基础设施命令由 `cargo xtask` 读取 `deploy/ansible/ansible.cfg`、清单、vault 文件和当前环境变量。控制器不是 Linux 时，`preflight --infra`、应用基础设施部署等入口应在批准的 Linux 控制器执行。

### 1.2 清单、运行标识和 kubeconfig

将示例清单复制到被 gitignore 的私有清单位置，并填写真实节点、维护用户、密钥和分组；不要把私有拓扑写回 `hosts.yml.example`。必须存在真实 inventory 和 vault password file：

```sh
test -f deploy/ansible/inventories/v1/hosts.yml
test -f deploy/ansible/inventories/v1/.vault-password
export LABWEAVER_RUN_ID="<lowercase-run-id-or-uuidv7>"
export LABWEAVER_KUBECONFIG="$PWD/.private/kubeconfig-v1-admin.conf"
export LABWEAVER_PLATFORM_REGISTRY="<registry-host>"
export ANSIBLE_VAULT_PASSWORD_FILE="$PWD/deploy/ansible/inventories/v1/.vault-password"
```

`LABWEAVER_RUN_ID` 用于隔离部署工作目录和验证资源，应用部署、基础层 reconcile、验证和身份 foundation 均应使用本次唯一标识。身份 foundation 的运行标识还必须满足 `infra-` 前缀要求。

清单至少应声明 `routers`、一个 `control_plane`、至少两个 `workers` 和一个 `nfs_servers`。`00-preflight.yml` 会检查占位符、操作系统、SELinux、KVM、NFS 和路由器条件；GPU 驱动、设备插件和 KubeVirt mediated device 另按第 5 节检查。

### 1.3 公共入口和模型服务

公网域名由 `deploy/ansible/roles/public_ingress` 的 `public_ingress_routes` 管理。Web、Keycloak、Harbor 和 OpenSSH Gateway 使用各自声明的 HTTPS/SSH 入口；用户从 HTTPS 首页开始，登录回调必须返回同一公共入口。浏览器验收保持证书校验开启，不使用内部端口、HTTP 绕过或 `LABWEAVER_IGNORE_HTTPS_ERRORS=1`。

Agent 的模型输入在 platform bundle 的 `agent-service-config/anthropic-base-url` 和 `anthropic-model` 中配置。当前候选部署固定使用集群内 Ollama Service 直连和 `qwen3.6:35b`，不经过旧代理，也不回退云模型。修改模型时重新渲染 bundle 并按第 4 节部署，不能只改 Pod 环境变量。

## 2. 首次安装和基础层

### 2.1 预检

使用 `xtask` 运行仓库实际的 preflight 入口：

```sh
cargo xtask preflight --env v1 --infra
```

只读检查结果后再进行任何节点变更。手工 Ansible 入口是 `deploy/ansible/playbooks/00-preflight.yml`；需要额外指定范围时使用现有 playbook 支持的变量，不自行添加新的校验脚本。

### 2.2 新集群基础安装

仅在目标集群尚未安装 Kubernetes、网络、存储、KubeVirt、sandbox runtime 和 public ingress 时执行完整站点 playbook。该入口依次导入 `00-preflight.yml`、节点准备、Kubernetes、网络、存储、KubeVirt、`75-sandbox-runtime.yml`、addons 和 `82-public-ingress.yml`：

```sh
ANSIBLE_CONFIG=deploy/ansible/ansible.cfg \
ansible-playbook \
  -i deploy/ansible/inventories/v1/hosts.yml \
  --vault-password-file deploy/ansible/inventories/v1/.vault-password \
  deploy/ansible/playbooks/site.yml
```

已有集群的应用升级不要重复执行整站 playbook；使用第 4 节的 profile 入口。`cargo xtask deploy --env v1 --infra --yes` 对应 `95-harbor.yml`，只用于 Harbor 安装或维护，不是完整集群安装入口。

### 2.3 持久化服务、BuildKit、Harbor 和身份

在已有 Kubernetes 上首次接入应用时按依赖顺序执行幂等 reconcile：

```sh
cargo xtask platform-foundation --env v1 --infra --yes
cargo xtask platform-buildkit --env v1 --infra --yes
cargo xtask platform-harbor-route --env v1 --infra --yes
```

这些入口分别对应 `92-platform-foundation.yml`、`92-platform-buildkit.yml` 和 `92-platform-harbor-route.yml`。Harbor 本身不存在时才使用 `cargo xtask deploy --env v1 --infra --yes`。

Keycloak foundation 使用受保护的 root-only locator；locator 由私有清单提供，不能把内容、固定 UUID 或口令写进文档：

```sh
export LABWEAVER_IDENTITY_SECRET_LOCATOR="<private-identity-bootstrap-locator>"
cargo xtask identity-foundation --env v1 --infra --yes --action deploy
cargo xtask identity-foundation --env v1 --infra --yes --action verify
```

身份基础层只 reconcile 需要的 realm、client、角色和用户，不删除已有身份。账号开通、角色授予和回收通过管理流程完成；普通用户从公共首页登录。

教师按姓名或用户名搜索组织账号，依赖 Access 服务账户在 `realm-management` 下的 `query-users`、`view-users` 角色及对应 client scope 映射。应用镜像部署不会自动更新身份基础层。已有环境升级涉及服务权限时，应执行已审核的身份配置 reconcile；目录查询不需要 `manage-users` 或 realm 管理员权限。页面出现 `LW_ACCESS_DIRECTORY_UNAVAILABLE` 时检查这两项映射及服务令牌刷新，再从教师页面重试搜索，不能直接写项目成员记录来跳过故障。

## 3. 配置 bundle、镜像和迁移

### 3.1 渲染配置

`deploy/config/platform-bundle-manifest.json` 和 `resource-bundle-manifest.json` 是 bundle 的唯一字段清单。输入目录必须位于受保护的 `.private` 目录；渲染器拒绝缺文件、额外文件、符号链接、过大文件以及明文输出到仓库：

```sh
python tools/render_platform_bundle.py \
  --input "$PWD/.private/v1/platform-application/render-input" \
  --output "$PWD/.private/v1/platform-application/configuration-bundle.yml"

python tools/render_resource_bundle.py \
  --input "$PWD/.private/v1/resource-application/render-input" \
  --output "$PWD/.private/v1/resource-application/configuration-bundle.yml"
```

平台输入必须包括 manifest 所列的 ConfigMap/Secret 文件；Resource 输入必须包括 `capacity.json`、`http.yaml` 和 Resource 服务所需 Secret。首次 bootstrap bundle 中的 `capacity.json` GPU seed 必须与 `deploy/versions.lock.yml` 的 `platform_gpu_classes` 逐字段一致；运行时目录以 Resource catalog 和管理员 UI 的业务记录为权威，lock 文件不是动态目录的第二真源。`environmentHandoff.systemActorId` 只从部署生成的 `environment-service-secrets/system-actor-id` 读取，不能复制示例 UUID 或在命令行临时改写。

渲染器使用独占创建，不覆盖已有输出；配置变更应使用新的、受保护的 bundle 文件名，并在 profile 部署后删除不再引用的临时副本。

### 3.2 打包和校验

打包要求源码树干净，并使用锁定的工具和 digest 镜像：

```sh
cargo xtask package --env v1 --release <platform-release> --profile platform --yes
cargo xtask package --env v1 --release <resource-release> --profile resource --yes

cargo xtask package-validate \
  --manifest <platform-package-manifest> --mode static
cargo xtask package-validate \
  --manifest <platform-package-manifest> --mode connected --env v1
cargo xtask package-validate \
  --manifest <resource-package-manifest> --mode static
cargo xtask package-validate \
  --manifest <resource-package-manifest> --mode connected --env v1
```

`platform` profile 包含除 Resource 外的服务；`resource` profile 只包含 Resource。不要使用可变 tag 代替 manifest 中的 digest，也不要用本地构建结果跳过 connected 校验。

### 3.3 迁移边界

`migrations/catalog.yaml` 是迁移目录唯一真源。每个业务 schema 的在线 migration ledger 必须是该目录的有序前缀，新增迁移只能追加。部署失败时先读取服务诊断和 ledger，不能删表、清空 schema 或手工改 migration 状态来绕过校验。数据库迁移前向执行；需要数据恢复时使用经批准的数据库恢复方案，不把 Helm 回滚当作数据库回滚。

## 4. 增量部署和回滚

### 4.1 Platform profile

`93-platform-application.yml` 是 platform profile 的唯一应用入口。它会检查现有服务、配置 bundle、Harbor/Keycloak/MinIO/NATS、迁移和 Access seed，然后以 `helm upgrade --install ... --wait` 更新应用。就绪检查或超时失败后，保留 Helm release 和集群状态；先读取 Helm、Pod、Service 及服务诊断，完成修复后重新 apply，不把旧应用自动回退当作数据库迁移的恢复方案。执行前设置对应的私有 locator：

```sh
export LABWEAVER_APPLICATION_VARS_FILE="$PWD/.private/v1/platform-application/application-vars.yml"
export LABWEAVER_POSTGRES_SERVICE_FILE="$PWD/.private/v1/platform-application/postgres-service.conf"
export LABWEAVER_POSTGRES_CLIENT_CERTIFICATE_FILE="$PWD/.private/v1/platform-application/postgres-client.crt"
export LABWEAVER_POSTGRES_CLIENT_PRIVATE_KEY_FILE="$PWD/.private/v1/platform-application/postgres-client.key"

cargo xtask platform-application --env v1 --infra --yes \
  --package-manifest <platform-package-manifest>
```

### 4.2 Resource profile

`94-resource-application.yml` 只管理 Resource workload、容量配置和它的 Kubernetes API egress；它显式关闭 platform profile 的共享 identity proxy，不使用 `--take-ownership`：

```sh
export LABWEAVER_RESOURCE_CONFIGURATION_BUNDLE="$PWD/.private/v1/resource-application/configuration-bundle.yml"
export LABWEAVER_ACCESS_SEED_FILE="$PWD/.private/v1/platform-application/access-seed.json"
export LABWEAVER_RESOURCE_VALUES_FILE="$PWD/.private/v1/resource-application/values.yml"
export LABWEAVER_POSTGRES_SERVICE="platform-admin"
export LABWEAVER_POSTGRES_SERVICE_FILE="$PWD/.private/v1/platform-application/postgres-client.conf"

cargo xtask resource-application --env v1 --infra --yes \
  --package-manifest <resource-package-manifest>
```

两个 profile 都要求 `LABWEAVER_RUN_ID`，并会检查输入文件存在、namespace、ConfigMap/Secret 键和 digest。按 profile reconcile 的操作约定，重复施加同一 manifest 应保持幂等；状态不明时先查询 Helm、Pod、Service 和业务页面，不要创建第二个 release 或第二个 migration run。

### 4.3 发布后检查

应用 profile 完成后使用仓库验证入口：

```sh
cargo xtask verify --env v1 --infra --yes
cargo xtask contracts check
```

验证资源使用自己的运行标识并在结束时清理。另行确认公网 HTTPS、Keycloak 回调、Web 上传、浏览器终端、VM 控制台、OpenSSH Gateway、六个服务的就绪状态，以及当前 bundle 中的 model、provider binding、GPU catalog 和费率版本。

### 4.4 回滚和停止边界

- 查看应用 revision：`helm -n labweaver-system history labweaver` 和 `helm -n labweaver-system history labweaver-resource`。
- 升级失败不会自动回退；先保留失败状态并完成诊断、修复和重新 apply。只有操作者明确选择与当前数据库契约相容的应用 revision 时，才可以经负责人确认使用 `helm rollback`；回滚前恢复对应的源码、镜像 manifest 和配置 bundle。
- 数据库迁移、对象存储版本、业务账目和已发布实验不会因 Helm 回滚而回退。迁移不兼容时停止发布并安排数据处理。
- 不使用不存在的 `xtask backup`、`xtask rollback` 或“清空后重装”作为日常恢复手段。删除 PVC/PV、数据库、NATS、MinIO、Keycloak 或共享 NFS 需要单独的负责人确认。
- 应用升级不重启节点、不切换共享存储、不删除仍被租约或环境记录引用的资源。

## 5. GPU 目录、观测和节点切换

### 5.1 模式和真实资源名

GPU 目录中的模式、provider binding 和 allocation binding 必须来自实际部署：

| 模式 | 运行时 | 资源/设备绑定 | 语义 |
| --- | --- | --- | --- |
| `exclusive` | 容器 | 设备插件的 `nvidia.com/gpu` | 一份申请独占整卡资源 |
| `container_time_slice` | 容器 | 设备插件的 `nvidia.com/gpu.shared` | 一个共享时间片，不能表示独占显存或固定比例算力 |
| `vm_vgpu` | KubeVirt VM | 已配置的 mediated device resource name | 依赖实际 mdev、驱动和许可证 |

当前配置中的容器 provider binding 是 `container-primary-v1`，VM provider binding 是 `kubevirt-primary-v1`；以当前 platform bundle 为准。真实 observer 的 key 就是 `gpuObservers[].providerBinding`，必须与对应 GPU catalog entry 完全一致，否则 Resource 只读观测不会被用于准入。观测器的实际 Kubernetes API server、ServiceAccount token 文件和 CA 文件来自私有 bundle，不在文档中固定地址或凭据。

`deploy/versions.lock.yml` 当前只提供 reviewed 的 `nvidia-cuda` 独占 bootstrap seed（`container-primary-v1`、`nvidia.com/gpu`），它不是运行时 GPU catalog 的第二真源。新增时间片或 VM vGPU class 前，先在设备插件/KubeVirt 和 Resource 观测中配置真实容量，再由管理员 UI 创建目录项和费率；缺失费率时报告缺失项，不覆盖现有活动费率 revision，也不把未观测容量当作可用。

### 5.2 设备插件和 mediated device

设备插件角色默认关闭，启用时必须给出互不重叠的节点集合。角色入口和变量名如下：

```sh
ansible-playbook \
  -i deploy/ansible/inventories/v1/hosts.yml \
  --vault-password-file deploy/ansible/inventories/v1/.vault-password \
  deploy/ansible/playbooks/80-install-addons.yml \
  --tags gpu-device-plugin \
  -e gpu_device_plugin_enabled=true \
  -e 'gpu_device_plugin_exclusive_nodes=["<exclusive-node>"]' \
  -e 'gpu_device_plugin_shared_nodes=["<shared-node>"]' \
  -e gpu_device_plugin_shared_replicas=10
```

变量默认值固定为 `nvidia.com/gpu`、`nvidia.com/gpu.shared`，共享副本数至少为 2；同一节点不能同时出现在 exclusive 和 shared 列表。切换到 VM vGPU 前，使用 `70-install-kubevirt.yml --tags kubevirt-mdev` 和受保护的 mdev 变量文件；该 tag 只更新明确的 KubeVirt CR 配置，不提供软件模拟：

```sh
ansible-playbook \
  -i deploy/ansible/inventories/v1/hosts.yml \
  --vault-password-file deploy/ansible/inventories/v1/.vault-password \
  deploy/ansible/playbooks/70-install-kubevirt.yml \
  --tags kubevirt-mdev \
  -e @deploy/ansible/inventories/v1/gpu-mdev-private.yml
```

### 5.3 从 exclusive 切换 shared 或恢复

节点模式切换是维护操作，不能用某个节点名作为永久配置。每次切换前：

1. 在管理员资源页面确认对应 provider binding 没有 `active` lease、待批准申请或仍在释放的租约；停止请求只有在业务记录确认释放后才算完成。
2. 只读检查所有工作负载的 GPU 扩展资源请求，确保 exclusive、shared、VM vGPU 的运行 Pod 都已结束。Environment Pod 本身不以 GPU 模式标签区分，不能只检查设备插件 DaemonSet：

   ```sh
   kubectl get pods -A -o json | python -c '
   import json, sys
   for pod in json.load(sys.stdin).get("items", []):
       resources = set()
       for container in pod.get("spec", {}).get("containers", []):
           for field in ("requests", "limits"):
               resources.update(container.get("resources", {}).get(field, {}))
       if any(key.startswith("nvidia.com/") for key in resources):
           print(pod["metadata"].get("namespace"), pod["metadata"].get("name"), pod.get("status", {}).get("phase"))
   '
   ```

3. 保存当前 inventory/变量文件中的 exclusive/shared 列表以及当前 `gpuCatalog`、费率 revision 和 observer 配置。确认没有活动 lease 和 GPU Pod 后，才用第 5.2 节 playbook 施加新的互斥节点集合。
4. 只读确认节点标签、设备插件 DaemonSet、扩展资源名和 observer 状态，再由管理员 UI 开放对应目录。没有通过 readback 前，Resource 必须保持不可用。
5. 测试结束后确认测试租约已释放、已知用量已结算，再从管理员费用页面为试用费率“安排结束”；结果不明先查询原费率，不能改截止时间另发请求。使用保存的原始列表重新施加 playbook，恢复原节点标签、插件配置、catalog active 状态和 observer 设置；再次确认没有活动 lease、没有遗留 GPU Pod，最后从 UI 恢复用户申请。

切换过程不重启节点、不清空共享卷。时间片只提供调度份额，不能承诺显存隔离；GPU 运行必须由用户实际执行 CUDA 数值计算确认，设备名称或 Pod `Running` 不足以宣称可用。

### 5.4 容量 observer 和失败关闭

`resource-service-config/capacity.json` 为每个真实 provider binding 配置 `gpuObservers`，每项必须使用 HTTPS API server、绝对的受保护 token/CA 文件、有限的 timeout、TTL（不超过 300 秒）和 node/pod 上限。Resource 只读 Node 状态、Pod 调度和自身 reservation，不创建、修改或删除 Kubernetes 对象。

缺少 observer、观测过期、GPU Pod 归属不匹配、释放未确认或实际容量小于目录声明时，申请应保持等待或明确失败并显示诊断；不能换成普通容器、Mock、另一种 GPU 模式或把未知容量当作零。重复或乱序用量事件不能重复计费。

## 6. Sandbox、评测和 VM

### 6.1 Sandbox 的适用范围

`75-sandbox-runtime.yml` 和 `sandbox_runtime` role 提供 `labweaver-sandbox` RuntimeClass，供 Agent authoring attempt 和 Ansible configuration probe 使用。它由 containerd + gVisor `runsc` 实现；CRI-O 不能提供该按 Pod RuntimeClass 边界。启用后只读确认：

```sh
kubectl get runtimeclass labweaver-sandbox -o jsonpath='{.handler}'
kubectl get nodes -o wide
```

OJ 程序评测使用节点默认 OCI runtime、`hostUsers: false`、用户命名空间和 Landlock；OJ 不应被描述为 gVisor 运行。若 OJ 需要用户命名空间、idmap mount、`/dev/null` 或运行时能力，按 `docs/deployment/ansible.md` 和评测服务配置排查，不能通过切换到 gVisor 绕过失败。

### 6.2 KubeVirt、VM base 和大镜像

KubeVirt 使用 `70-install-kubevirt.yml`，必须关闭软件模拟并满足 `/dev/kvm`、CPU 虚拟化、mdev 和 CDI 前置条件。平台 bundle 中的 VM provider 当前为 `kubevirt-primary-v1`，base catalog 由 `deploy/versions.lock.yml` 与 platform bundle 一起校验：

```sh
kubectl -n labweaver-system get dv,datasource
kubectl -n kubevirt get kv kubevirt
```

大 VM 镜像导入、查询进度和取消都从管理员公共首页的镜像任务入口完成。取消后继续查看原任务，直到页面显示已取消和清理完成；状态不明时刷新原记录，不重复导入。VM vGPU 还要在用户环境中检查驱动、设备和许可证，许可证缺失时保持失败关闭。

## 7. 公共用户验收

验收从公共 HTTPS 首页开始，每个角色使用独立会话。首次进入角色工作台应点击首页展示的任务卡；刷新或恢复已有任务时可以返回原任务链接。验收不关闭 TLS 检查、不直接写 API/数据库补齐用户无法完成的步骤，不保留截图、trace、video 或单独结果文件。

### 7.1 验收入口

`tools/user_acceptance.py` 只在受保护凭据目录存在且公共入口可达时运行。模型必须显式选择：

```sh
python tools/user_acceptance.py preflight \
  --base-url "$LABWEAVER_BASE_URL" \
  --credentials-dir "$PWD/.private/labweaver-acceptance/credentials" \
  --model qwen3.6:35b

python tools/user_acceptance.py run \
  --base-url "$LABWEAVER_BASE_URL" \
  --run-id "<new-uuidv7>" \
  --journeys work,admin \
  --provider-binding container-primary-v1 \
  --model qwen3.6:35b \
  --credentials-dir "$PWD/.private/labweaver-acceptance/credentials"
```

runner 会把口令、认证状态和 Playwright 临时输出放在临时目录并在退出时删除。公开入口配置固定 `retries=0`；本轮验收对明确失败的 Agent task 最多执行一次人工重试，传输结果不明时先查询而不是重复提交。

### 7.2 需要逐项完成的界面流程

- **管理员**：从首页进入配置工作台，配置模型选项、容器/VM 镜像、GPU 目录、费率和资源审批；从镜像任务页导入、查询并取消大镜像；在审批页查看项目、申请人、规格、lease、用量和费用。费用页面可为开放费率安排未来截止时间，区分“已安排结束”与“已结束”；此操作影响全平台匹配用量，不会停止环境或修改历史账单。GPU catalog 和 rate 的 mutation 只通过管理员 UI。
- **教师**：创建或选择项目，在项目 AI 设置选择当前允许的模型和预算；在材料页上传题面、源码、样例和环境要求；启动 Agent、构建候选并审核；在成员页用组织账号的显示名称或 `username` 搜索并加入学生，不从另一会话读取 actor ID；审批并发布版本。刷新后从任务历史继续同一任务。
- **学生**：从项目进入已发布实验，申请并启动环境；在浏览器终端、VM 控制台或 SSH 中实际修改源码/guest 配置；冻结提交，查看确定性成绩和反馈；停止后区分“停止中”“释放中”“已释放”，最终从页面释放环境。
- **个人 Work**：使用同一账号创建项目和模板，生成、审核并发布；申请容器或 VM，配置软件，执行实际任务；停止/重启后按模板和页面的数据保留说明检查工作区，需要保留的软件配置应写入模板声明的持久化路径，最后释放并查看费用。私人项目负责人通过受邀者的准确 username 管理已知成员，不需要切换管理员身份。
- **GPU**：分别选择管理员已开放的独占、容器时间片和 VM vGPU class，运行真实 CUDA 数值计算；VM 同时检查驱动和许可证。容量不足时等待或明确拒绝，不能自动换模式。
- **访问**：用户在 SSH 公钥页面登记公钥，环境授权后从环境页面复制完整 SSH 命令，实际连接、断开后重连，再从 UI 撤销授权并确认旧命令失效。测试脚本必须走复制页面命令这一步，不能只拼 API endpoint。

### 7.3 成员、任务和生命周期语义

界面应同时显示项目名称、任务名称、当前状态、下一步、审批对象、等待原因和处理人。内部 UUID、provider、revision 和详细诊断放在高级详情。长任务刷新后应保留任务历史、取消和可恢复失败草稿；重试同一任务应避免重复提交，实际新增用量仍可能产生费用，重复或乱序事件不得重复入账。停止请求不等于释放，释放中的用量仍待确认，已释放后才允许容量再次分配。

## 8. 故障排查和安全清理

应用回滚必须使用与当前数据库契约相容的版本。统一教学计量迁移将旧 GPU 预留表改为环境资源授权，并更新用量目标与实例授权快照；旧二进制不能直接跨这次结构变更回滚。部署前完成迁移与受影响行为测试，先在相容版本之间验证候选应用回滚，再执行结构升级。数据库恢复会影响共享业务记录，须另行确定对象与恢复边界，不能用普通 Helm 回滚替代。

这次契约升级前，先确认受影响服务没有未发布 outbox，Environment 生命周期及访问状态消费者没有待处理或待确认消息。若仍有旧契约任务，先让当前版本完成处理并等待队列排空；不能清空流或手工确认消息来绕过处理。升级期间暂停发起新的环境和 Agent 任务，全部受影响服务就绪后再恢复用户操作。

先在公共页面刷新原项目/任务，再按下面边界分类；只读 Kubernetes 查询用于诊断，业务 mutation 仍从正常 UI 完成：

| 现象 | 首先检查 | 处理边界 |
| --- | --- | --- |
| HTTPS 或登录回调失败 | 公共 Gateway、Certificate、Keycloak redirect 和浏览器证书 | 修正 ingress/identity 配置后重新部署；不关闭证书检查 |
| 模型任务失败 | bundle 中 base URL/model、Ollama Service、Agent 任务状态和诊断 | 区分模型、任务执行、资源和平台问题；本轮验收只对明确失败任务人工重试一次 |
| 资源一直等待 | Resource UI 的申请、active lease、容量观测 TTL 和 provider binding | 不把未知容量当作零，不创建重复申请 |
| 容器/VM 未就绪 | Environment 状态、provider、Pod/VMI、PVC/DataVolume 和 release | 等待原对象或按页面取消；不手工创建第二个环境 |
| 评测失败 | Evaluation run、OJ runtime 配置、收据和用户提交版本 | 保留确定性错误和反馈；不以固定分数或 Mock 替代 |
| SSH 不能连接 | UI 授权状态、复制的 host/port/命令、host key 和 Gateway | 从 UI 重新授权或撤销；不共享私钥、不绕过 Gateway |
| 费用异常 | Resource 用量事件、费率 revision、重复/乱序事件和调整记录 | 追加带原因的调整，不覆盖原账目；停止不自动记为释放 |

测试资源清理遵循“先查业务记录，再查 namespace/Pod/PVC，最后由 UI 释放”。只有确认没有 active lease、运行 Pod、待处理任务和未结算用量后，才可以按精确名称清理临时 Kubernetes 对象；不得使用 `--all`、模糊 label 或删除共享数据卷。保留正常业务账目和必要诊断，清除已过期的临时认证、浏览器输出和本地凭据副本。

### 8.1 常见部署诊断

以下诊断码来自当前 playbook/服务实现，可直接用于定位输入边界：

- `XTASK_INFRASTRUCTURE_REQUIRED`、`XTASK_CONFIRMATION_REQUIRED`、`XTASK_INFRA_UNSUPPORTED_PLATFORM`：命令缺少 `--infra`、`--yes` 或不在批准的 Linux 控制器执行。
- `LW_PACKAGE_INPUT_DIRTY`、`LW_PACKAGE_PROFILE_MISMATCH`、`LW_PACKAGE_MANIFEST_INVALID`：源码、profile 或 package manifest 不满足锁定约束。
- `IDENTITY_CONFIGURATION_INVALID`、`IDENTITY_SECRET_LOCATOR_INVALID`、`IDENTITY_BOOTSTRAP_SECRET_INVALID`：身份私有输入、locator 或 `infra-` run id 不正确。
- `PLATFORM_APPLICATION_CONFIGURATION_INVALID`、`PLATFORM_APPLICATION_CONFIGURATION_KEYS_INVALID`、`PLATFORM_APPLICATION_GPU_CLASS_CATALOG_MISMATCH`：platform bundle、Resource bundle 或 GPU seed 与当前 manifest/lock 不一致。
- `RESOURCE_APPLICATION_INPUT_INVALID`、`RESOURCE_APPLICATION_REQUIRED_PATH_MISSING`：Resource profile 所需私有文件、values 或 PostgreSQL service file 缺失。
- `VERIFY_EXECUTION_INPUT_INVALID`、`VERIFY_CLEANUP_FAILED`：验证输入或验证资源清理失败；先按运行标识查找精确资源。
- `GpuObservationStale`：GPU observer 缺失、过期或未读到完整容量；保持失败关闭并修正真实 observer。

不要把一次浏览器验收的运行编号、临时 Pod 名称或诊断复制成永久配置。产品状态、费用和用户反馈保留在正常业务记录以及对应 Issue/PR 中。
