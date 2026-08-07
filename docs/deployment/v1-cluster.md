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
