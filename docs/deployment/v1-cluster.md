# LabWeaver v1 三节点 GPU 集群部署说明

> 对应 Issue：[#148 feat(deploy): LabWeaver v1 three-node GPU cluster and release
> formalization](https://github.com/TeamMonad/LabWeaver/issues/148)（High risk，禁止
> auto-merge；Owner D `@Nova-Lciop-J`，Core reviewers A/B）。
> 本文只记录公开事实、受控 locator 与验证步骤；**任何密码、token、私钥内容、
> Secret 值不进入 Git**。

## 1. 节点拓扑与事实基线（2026-08-07 采集）

| 角色 | 节点名（当前 hostname） | IP | OS / 内核 | CPU/RAM | GPU（UUID） | 特殊能力 |
| --- | --- | --- | --- | --- | --- | --- |
| 控制节点（K8s CP + LLM Provider） | `dase` | 49.52.27.63 | Ubuntu 22.04.5 / 5.15.0-181 | 48C / 125G | RTX 3090 ×2（`GPU-d64f9bdc-…`、`GPU-731563f2-…`） | `/dev/kvm` ✓；ollama 0.30.11 已装（systemd active）；无 K8s 残留 |
| Worker（V100） | `dell-PowerEdge-R740`（与 158 重复） | 49.52.27.97 | Ubuntu 22.04.5 HWE / 6.8.0-124 | 32C / 157G | Tesla V100-SXM2-32GB（`GPU-fab846f6-…`） | `/dev/kvm` ✓；ollama 0.30.8 active，`qwen3.6:35b` 常驻 GPU；**活跃旧 k8s 残留**（见 §5） |
| Worker（P40 mdev） | `dell-PowerEdge-R740`（与 97 重复） | 49.52.27.158 | Ubuntu 22.04.5 HWE / 6.8.0-136 | 32C / 157G | Tesla P40（`GPU-b6c4b09f-…`） | `/dev/kvm` ✓；**VGPU 驱动已装，mdev bus `0000:3b:00.0`**；ollama 0.30.8 active；活跃旧 k8s 残留 |

- SSH host key 指纹（ed25519，用于 known_hosts 校验）：
  - 63 `SHA256:iZxubtUdYgCLTkV2QXp0HsNGZhozTCO0W507OeSpSEQ`
  - 97 `SHA256:oG7amsxj18gTcrcnCi8GYbNfG9iMq0qnx2axIjGyuMo`
  - 158 `SHA256:X40SiAeT5rnBK0MLg3X/YFbTPCGOF0OJ4BY7/KSHLlc`
- 完整指纹集（ed25519/ecdsa/rsa）见凭据 registry（§4）。

## 2. 维护账户 `labweaver-deploy`

按 #148 范围在三节点创建，策略：

| 项目 | 值 |
| --- | --- |
| 用户名 | `labweaver-deploy`（63 uid=1001；97/158 uid=1002） |
| 登录方式 | **仅 SSH key**（OpenSSH `Match User` 强制关闭 password/kbd-interactive） |
| sudo | `ALL=(ALL) NOPASSWD:ALL`（`/etc/sudoers.d/labweaver-deploy`，0440 root:root） |
| 凭据目录 | `/var/lib/labweaver/.private`（root:root **0700**） |
| 策略文件 hash | sudoers `911700e6…7616be`；sshd drop-in `d6f34f04…217dc293`（三节点一致） |

- 专用控制器密钥对（**不落 Git**）：`.private/v1-deploy/controller-key`（0600
  ACL 收紧）+ `controller-key.pub`；公钥指纹
  `SHA256:Vz0P2tiX9Y1mwlCpvWK+smXqNwQ6UMfXx2zHRE3tSm0 labweaver-v1-controller`。
- 本机 ssh 别名（`~/.ssh/config`，已备份）：`v1-cp-63` / `v1-worker-97` /
  `v1-worker-158`，user `labweaver-deploy`，`StrictHostKeyChecking yes` +
  固定 `UserKnownHostsFile .private/v1/baseline/known_hosts`。

```sh
ssh v1-cp-63 "id && sudo -n true && echo SUDO_OK"
```

## 3. 创建/复核维护账户

引导脚本（幂等）：`.tmp/bootstrap_maintenance.py`（仅 bootstrap 期使用，密码经 argv
传入、不回写）。人工复核命令（任意节点）：

```sh
id labweaver-deploy
sudo -n -u labweaver-deploy true   # 无密码 sudo
ssh-keygen -lf /home/labweaver-deploy/.ssh/authorized_keys  # 期望 labweaver-v1-controller
# fail-closed：密码认证必须被拒（OpenSSH 交互或 paramiko 均验证过）
```

## 4. 凭据登记与漂移检测（凭据完整、与集群对应、不漂移）

### 4.1 registry（root-only，控制节点 63）

```
/var/lib/labweaver/.private/v1-baseline/          # root:root 0700；文件 0600
  ├── registry.json          # 节点事实（OS/GPU/残留/服务状态）
  ├── host_keys.json         # 三节点 SSH host key 指纹（ed25519/ecdsa/rsa）
  ├── known_hosts            # controller 侧严格校验用
  ├── bootstrap.json         # 维护账户创建+复核记录
  ├── controller-key.pub     # 控制器公钥（指纹 Vz0P2tiX…）
  ├── v1-*.facts.txt         # 每节点事实快照
  └── manifest.sha256        # 上述全部文件的 sha256 清单
```

本地镜像（同一份内容）：`.private/v1/baseline/`。registry 只记 locator/hash/计数/
指纹，**不存任何 Secret 值**；丢失签发私钥的处理按 AGENTS.md 受控签发源规则轮换，
禁止从日志/Git/Secret 反推。

### 4.2 漂移检测（后续验证直接使用）

```sh
python .private/v1/scripts/drift_check.py \
  --key .private/v1-deploy/controller-key \
  --baseline .private/v1/baseline \
  --hosts v1-cp-63:49.52.27.63,v1-worker-97:49.52.27.97,v1-worker-158:49.52.27.158 \
  [--password <dase-password>]   # 追加 fail-closed 密码拒绝探针
```

检查项：维护账户 uid、sudoers/sshd 策略 hash、`.private` 权限、authorized_keys
指纹与数量、三节点 host key 指纹、GPU 拓扑（UUID 集合）、mdev bus（158）、可选
密码拒绝。输出全部 `[OK]` 且 `DRIFT_OK` 才算通过（2026-08-07 基线为全绿）。

## 5. 部署前提与已发现阻断项（先决条件，未满足前禁止动集群）

1. **OS 家族不匹配（硬阻断）**：现有 Ansible 栈（`00-preflight.yml` +
   `rocky_common` + `kubernetes_packages`）只支持 **Rocky/RHEL 10 + SELinux
   Enforcing + dnf**；preflight 硬断言 `ansible_facts.os_family == 'RedHat'` 且
   major ≥ 10。三节点为 **Ubuntu 22.04**（`/etc/sudoers.d` 0700、无 SELinux
   Enforcing、apt 体系）。→ 需要 v1 Ubuntu 部署路径（新分支），或重装 Rocky
   （需带外管理，且会丢失 63 的 ollama 与 158 的 VGPU 驱动环境）。
2. **97/158 活跃旧集群残留**：kubelet + crio 均 active；`/etc/kubernetes`、
   `/var/lib/kubelet`、`/var/lib/crio`、`/etc/cni/net.d` 存在；有 `lxc_*` 接口与
   Cilium 路由（10.0.0.0/24、10.0.1.0/24）。新集群 bootstrap 前必须 `kubeadm
   reset` + 清理并确认无保留依赖（LXD 容器需逐一确认归属）。此项属于破坏性操作，
   按 #148 deployment window boundary 需显式验收窗口。
3. **hostname 重复**：97 与 158 均为 `dell-PowerEdge-R740`，K8s 要求节点名唯一；
   需重命名（建议 `v1-worker-97` / `v1-worker-158`，63 → `v1-cp-63`）并同步
   `/etc/hosts`。
4. **ollama 三节点均已运行**：63 为 v1 LLM Provider 绑定目标（Agent Service 只读
   三个通用字段 `ANTHROPIC_BASE_URL` / `ANTHROPIC_AUTH_TOKEN` / `ANTHROPIC_MODEL`）；
   97/158 上的 ollama 是否保留需确认（97 的 `qwen3.6:35b` 正占用 V100，KubeVirt
   GPU passthrough 与 ollama 不能共享同一物理 GPU）。

## 6. 部署路径（待 A 决策后执行，见 §7）

- 阶段 0：验收窗口 + inventory/凭据 preflight（登记 §5 各项，D 复核）。
- 阶段 1：三节点 Ubuntu 准备（唯一 hostname、apt 包、sysctl/modules、swap off、
  97/158 旧残留清理，先确认 LXD/容器归属）。
- 阶段 2：CRI-O 1.35.5 + K8s v1.35.6（pkgs.k8s.io deb 源，digest 锁
  `deploy/versions.lock.yml`），63 控制面，97/158 加入。
- 阶段 3：Cilium 1.19.5（kube-proxy replacement、Gateway API）、MetalLB 0.16.1、
  local-path + NFS RWX（`nfs-rwx` StorageClass）。
- 阶段 4：KubeVirt 1.8.4 + CDI 1.65.0（仅硬件 KVM，`useEmulation: false`）；
  V100 GPU passthrough/device plugin；P40 mdev（`0000:3b:00.0`）真实 readback。
- 阶段 5：Ollama 绑定为 Anthropic-Compatible Runtime（Agent Service 三通用字段），
  真实 AgentRun。
- 阶段 6：PostgreSQL/NATS/MinIO/Harbor/Trivy/BuildKit/Keycloak + 平台十工作负载
  双幂等 reconcile + `verify` + Playwright `demo replay` + `release-gate`（映射
  #148 验收清单）。
- 阶段 7：凭据 registry 更新（本轮新增的 CA/JWT/Secret locator）、漂移复查、
  A+B 人工批准、D connected Verify。

## 7. 校园网门禁与阻断规则

- 三节点 22 端口可达性即校园网登录门禁（`connect_ex` 检查）。2026-08-07 全通。
- **登录失效（任一节点 22 端口不可达）→ 立即阻断所有远程操作，等待人工恢复**
  （人工确认校园网/账号状态后才可继续）；已完成的基线/registry 不因离线回滚。
- 任何节点重新开机、IP/主机名变更、host key 变化都会触发漂移检测失败，
  必须先人工核对再继续部署。

## 8. 安全边界（本集群所有操作必须遵守）

- 远程变更全部由受控控制器（专用密钥 + 严格 known_hosts）执行；`dase` 密码只
  用于 bootstrap 一次性引导，不回写任何文件。
- 私钥、JWT、Seed、`.creds`、Secret 内容禁止写入 Git、日志、issue、报告；
  报告只记录 locator/hash/计数。
- root-owned 目录 0700、文件 0600；`/var/lib/labweaver/.private` 为受控签发源
  根目录（NATS authority rotation、部署 bundle 的 locator 根）。
- 破坏性操作（旧残留清理、kubeadm reset、格式化）必须显式确认 + 验收窗口，
  与 #148「deployment window boundary」一致。

## 9. 部署执行记录（2026-08-07，分支 feature/148-v1-ubuntu-cluster）

> 本记录只登记已完成事实与受控 locator；**未完成的阶段不表述为已完成**。
> 控制器：63 root-only `/var/lib/labweaver/v1-controller`（git HEAD 见每次
> playbook 运行的 commit），Ansible venv `/opt/labweaver/venv`（ansible-core
> 2.18.6 + kubernetes 34.1.0 + jsonschema 4.24.0，与 controller.lock.yml 一致）。

### 9.1 已完成的部署阶段

| 阶段 | 内容 | 证据/状态 |
| --- | --- | --- |
| 0 | inventory/凭据 preflight | 00-preflight.yml failed=0；凭据 DRIFT_OK（见 §9.4） |
| 1 | Ubuntu 节点准备（唯一 hostname、swap off、sysctl、模块） | 15-v1-node-prepare.yml（此前已完成） |
| 1b | 97/158 旧残留审计+清理 | 15b-v1-residue-cleanup.yml；root-only 审计记录在 63 `/var/lib/labweaver/.private/v1/residue-audit-*.json` |
| 2 | CRI-O 1.35.5 + K8s v1.35.6（pkgs.k8s.io deb 源，精确版本 pin） | 20-install-crio-k8s.yml failed=0；三节点 `kubelet/kubeadm v1.35.6`、`crio 1.35.5` |
| 2c | 63 控制面（kubeadm init v1.35.6，skip kube-proxy） | 30-bootstrap-control-plane.yml；`/etc/kubernetes/admin.conf` |
| 2d | 97/158 加入（定向 reset 手动并发控制面后重新 join） | 40-join-workers.yml changed=2；三节点 Ready |
| 3 | Cilium 1.19.5（kube-proxy replacement + Gateway API + Hubble）、MetalLB 0.16.1（IPPool 10.99.0.100-200） | 50-install-network.yml failed=0；ds cilium 3/3；`gatewayclass cilium` Accepted |
| 3s | local-path（directory 模式）+ NFS CSI（nfs-rwx，158 导出 `/srv/nfs/k8s`） | 60-install-storage.yml failed=0；`local-path (default)` + `nfs-rwx` StorageClass |
| 4 | KubeVirt 1.8.4 + CDI 1.65.0（useEmulation: false，KVM 设备 kvm/tun/vhost） | 70-install-kubevirt.yml failed=0；`kv/kubevirt Deployed`；真实 VM 生命周期验证通过（见 §9.2） |
| 5a | cert-manager 1.21.0 + ClusterIssuer dev-selfsigned | 80-install-addons.yml failed=0 |
| 5b | 内部 Gateway（cilium 类，MetalLB 10.99.0.100）+ labweaver 命名空间 | 80-install-addons.yml；`gateway/public-gateway Programmed=True` |

### 9.2 真实 KubeVirt VM 生命周期验证（本记录唯一运行证据）

- 位置：63 root-only `/var/tmp/kvm-probe-vm.yml`（后已删除）；namespace
  `labweaver-verify-baseline`（验证后已删除，零残留）。
- 流程：DataVolume 导入 cirros（`source.http` 指向 63 本地 HTTP 8899，因
  CDI importer 直连 quay.io 被校园网阻断；cirros qcow2 走代理下载后提供）→
  `virtctl start` → VMI Running（节点 v1-worker-158，真实 KVM）→ console 连接
  rc=0 → `virtctl stop` → VMI 删除 → `virtctl start` → VMI 再 Running →
  delete → VMI/VM/pod/PVC 零残留。
- **待办**：V100 GPU passthrough（97）、P40 mdev（158 `0000:3b:00.0`，
  GRID P40 类型已确认）尚未配置 KubeVirt `permittedHostDevices`，属下一轮
  #148 阶段 4 收尾；不做任何 GPU 伪证据。

### 9.3 本部署发现的缺陷与修复（全部在 feature/148-v1-ubuntu-cluster 分支提交）

- `kubernetes_packages`：Ubuntu 分支无 `startswith` Jinja test → 用 `match`；
  apt 候选随通道演进（1.35.7/1.35.6）→ 改为精确版本 pin + madison 断言；
  CRI-O 代理 drop-in（校园网 egress）+ 清理残留 7891 drop-in。
- `control_plane`：kubeadm init 补 `--kubernetes-version`（防 fallback 漂移）。
- `cluster_network`：Gateway API 从 v1.6.0 standard 降级 **v1.4.1 experimental**
  （Cilium 1.19 需要 TLSRoute v1alpha2 served；v1.6 standard 移除之）；
  本地 CRD 文件；helm/k8s 模块代理环境变量（仅 helm 走代理，k8s API 客户端不走）。
- `storage_nodes/controllers`：v1 directory 模式（跳过 device/SELinux）；本地
  local-path manifest；NFS CSI helm 代理。
- `kubevirt`/`cluster_addons`：本地 manifest 文件（GitHub raw 校园网不可达）。
- 并发控制面清理：20:31 手动 init 的 97 控制面（dase 从 172.20.153.109，
  aliyun 镜像源）与 158 的 join 状态按用户决策 reset，重新 join 63。

### 9.4 凭据与漂移复查（部署完成后 2026-08-07）

- `drift_check.py`（`.private/v1/scripts/`）：三节点 `[OK]`，`DRIFT_OK`。
  维护账户 uid/sudoers/sshd hash、authorized_keys 指纹、`.private` 0700、
  host key、GPU 拓扑（63 双 3090 / 97 V100 / 158 P40+mdev）全部与 2026-08-07
  基线一致；**本次部署未触碰任何签发源/私钥/凭据**。
- 控制器密钥指纹（63 `/root/.ssh/controller-key` = 本机
  `.private/v1-deploy/controller-key`）：`SHA256:Vz0P2tiX…tSm0`。
- 新增 bundle（root-only，63 `/var/lib/labweaver/bundle/`）：gateway-api
  experimental v1.4.1、local-path v0.0.36、kubevirt/cdi operator+cr、
  cert-manager v1.21.0、cirros qcow2（验证用，已停 HTTP 服务）。

### 9.5 遗留与下一轮（不冒充已完成）

- **V100 GPU passthrough（97）与 P40 mdev（158）**：KubeVirt
  `permittedHostDevices`/`mediatedDevicesConfiguration` 未配置；97 上 ollama
  `qwen3.6:35b` 仍占用 V100，需先停用再绑定（§5.4 决策项）。
- **Sprint 组件（阶段 5/6）**：Ollama→Agent 绑定、PostgreSQL/NATS/MinIO/Harbor/
  Trivy/BuildKit/Keycloak、平台十工作负载未部署。
- **正式 verify/release-gate**：90-verify 依赖 TestFlight 身份与 Harbor/backup
  证据链，属 Sprint 结束验收窗口（A+B 批准 + D connected Verify），本记录
  不替代。Playwright `demo replay` 同样未执行。
- 97/158 上 ollama 是否保留、63 ollama 与 KubeVirt CPU 调度共存策略待 A 决策。
