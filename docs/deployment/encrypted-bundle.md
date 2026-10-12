# 加密应用 Bundle

本入口将 [Control 管理设置](../contracts/platform-configuration.md)、应用与 Resource profile 输入、Secret、版本和说明放入同一个加密目录。导入继续使用现有管理员条件更新 API。首装设置可以离线准备，不制造已保存版本。运行权限消费者和基础设施安装仍需接入。#203 保持开放。

## 配置归属

`tools/deployment_bundle.py` 读取一个受限目录中的 `config/bundle.yaml`。该文件引用同一目录中的服务配置、Secret、版本清单和运维说明。服务配置继续使用现有服务类型。`deploy/config/platform-bundle-manifest.json` 定义应用允许的对象和键，不保存安装值。

目录中的文件是输入。运行目录中的 ConfigMap/Secret、Helm values 和 Ansible vars 是派生输出。不要编辑派生输出，也不要将其用作下一次导出的输入。多个对象需要同一凭据时，引用同一个 Secret 文件。每个服务的 NATS 身份仍使用各自的 credential。

`configurationFile` 指向管理配置 JSON。`managed_version` 使用现有 `PlatformConfigurationVersion`。`initial_settings` 使用现有 `UpdatePlatformConfigurationRequest`，expectedRevision 必须为0。`xtask bundle-configuration` 复用 Control 的配置类型、默认值、策略校验和 canonical JSON 摘要。Python 不另写运行权限规则。首装请求不携带作者、时间、applied 或执行历史。

`application.workloadInputs` 是本 Bundle 的完整服务环境输入。Helm 使用该 map 直接替换环境变量，不从旧环境 defaults 补值。对象、文件挂载、Probe 和资源布局仍复用现有 chart。非 Bundle 调用者暂时保留旧路径，其配置不能作为本入口的隐含输入。

`93-platform-application.yml` 在载入 Bundle 派生的 vars 后读取 `platform_application_versions_lock_file`。该路径指向 Bundle 的版本锁。已有部署入口未指定该字段时，仍读取仓库的 `deploy/versions.lock.yml`。这保留现有调用者，不能证明它们已经采用加密入口。

可选 infrastructure profile 将静态 inventory 和凭据文件引用纳入同一目录。xtask 使用派生 inventory，缺少 infrastructure 时明确失败，不从另一份环境配置补值。旧凭据签发工具继续签发，Bundle 不另写 CA、NATS 或身份签发器。服务启动时仍执行现有配置类型的业务校验。本工具复用两个现有对象渲染器，校验输入、版本和各服务的既有 NATS 权限。

## 前提

- 使用支持 POSIX 文件权限的 Linux 或 macOS 控制器。Windows 使用受限的 Linux 控制器或 WSL。
- 安装 Python 和 `tools/requirements-local-dev.txt` 中固定的 PyYAML。
- 从同一仓库版本执行 `cargo build -p xtask --locked`。默认校验入口为 `target/debug/xtask`。自定义构建目录时设置 `LABWEAVER_BUNDLE_CONFIGURATION_TOOL` 为该二进制的绝对路径。缺少校验入口时明确失败。
- 安装官方 [age v1.3.2](https://github.com/FiloSottile/age/releases/tag/v1.3.2)。本仓库的 CI 校验 Linux amd64 归档 SHA-256 `cbe24006683f8eb669266162894b9a522a1af52f2665fbc63a4bb032ed26ac10`。
- 单独保管 age 解密身份文件。不要将它放入 Bundle 或 Bundle 运行目录。
- 用已有签发流程准备服务凭据。不要从日志、聊天记录或已部署 Secret 反向恢复私钥。

## 目录和模型

```text
.private/installation/
  config/bundle.yaml
  config/platform-configuration.json
  config/runtime/<object-name>/<key>
  secrets/<credential-file>
  versions/lock.yaml
  versions/package.json
  docs/operations.md
```

Bundle 目录、`.private` 目录及其内部目录必须由当前用户持有，权限不得向组或其他用户开放。文件使用 `0600`，目录使用 `0700`。工具拒绝符号链接、设备文件和路径越界。不要在普通目录中准备明文后再加密。

模型必须提供下列字段。未知顶层字段、重复 YAML 键和 YAML 别名会失败。

| 字段 | 规则 |
| --- | --- |
| `apiVersion` | 固定为 `deploy.labweaver.io/bundle/v1` |
| `revision` | 1 到 9223372036854775807 的整数。该字段是导出配置版本，不代表已应用版本 |
| `configurationFile` | `config/` 下的管理版本 JSON。其 revision 必须等于 Bundle revision，其摘要必须匹配同一 Rust 配置模型 |
| `configurationMode` | 未指定时为 `managed_version`。首装显式使用 `initial_settings`，Bundle revision 必须为1，该值尚不是 Control 版本事实 |
| `target.namespace` | Kubernetes DNS label |
| `target.releaseName` | Helm release 名称，使用 Kubernetes DNS label |
| `versions.lockFile` | `versions/` 下的文件。沿用 `schema_version: 1` 的部署版本锁 |
| `versions.packageFile` | `versions/` 下的构建 package JSON。镜像必须使用唯一组件名和 SHA-256 引用 |
| `docs.operationsFile` | `docs/` 下的非空运维说明 |
| `application.configMaps` | 对象名、键和 `config/` 下文件路径的映射 |
| `application.secrets` | 对象名、键和 `secrets/` 下文件路径的映射 |
| `application.helmValues` | 非秘密 Helm 输入。镜像和 `deploymentIdentity` 由工具派生 |
| `application.ansibleVariables` | 应用角色参数。只接受 `platform_application_` 前缀 |
| `application.workloadInputs` | chart 中每个 workload 的 `env` 和 `secretEnv`。启用的服务必须提供全部已声明必需键及所用镜像 |

`configMaps` 和 `secrets` 必须完整匹配应用 manifest 的对象与键。工具拒绝缺项和额外对象。所有文件都必须被模型引用，不能把未使用的私钥顺带加入归档。两个对象可以引用同一文件。

以下片段说明引用语法，不是完整部署输入：

```yaml
application:
  configMaps:
    control-service-config:
      config.yaml: config/runtime/control-service-config/config.yaml
  secrets:
    control-service-secrets:
      database-url: secrets/control-database-url
  helmValues:
    hostAliases: []
  ansibleVariables:
    platform_application_nats_credentials_file:
      file: secrets/platform-admin-nats.creds
    platform_application_minio_password:
      secret: secrets/minio-admin-password
```

`file` 引用产生受限运行目录中的绝对文件路径。`secret` 引用读取 UTF-8 文本并去掉末尾换行。工具用 Ansible `!unsafe` 标记该文本，阻止凭据中的 Jinja 表达式执行。不要将密码、Token 或私钥直接写入模型或 Helm values。

工具固定派生 namespace、release 名称、版本锁路径、package 路径、对象 manifest、配置对象和 Helm values 路径。模型不能覆盖这些派生字段。Helm 镜像来自同一 package 文件，配置摘要来自实际渲染的应用对象。

`env` 的值必须是字符串。Token、密码和私钥不得内联。`secretEnv` 使用 `{secret: 对象名, key: 键名}`，只能引用本 Bundle 已声明的 Secret 对象与键。同一个源文件可供多个对象使用。`helmValues` 不能再提供 `workloadInputs`，也不能覆盖 workload 的 env、secretEnv、configMap 或 secret。

主应用 profile 不管理 Resource workload。使用下述 `resource` profile 在同一归档中携带其输入，保留原独立 Helm release 与对象归属。

## 同一目录中的 Resource profile

在 `config/bundle.yaml` 中添加 `resource`。该字段不建立第二个配置目录或秘密仓库。

| Resource 字段 | 规则 |
| --- | --- |
| `target` | namespace/releaseName。不能与主应用使用同一个目标 release |
| `packageFile` | `versions/` 下已有 Resource package。profile 为 resource，恰好包含一个固定摘要的 resource-service 镜像 |
| `natsIssuer` | 已有 NATS 账户公钥。渲染器核对 Resource credential 的 issuer 与原有权限集合 |
| `configMaps` / `secrets` | 完整匹配现有 resource-bundle-manifest.json 的对象/键，使用本目录文件引用 |
| `helmValues` | 非秘密布局与网络输入。不能启用其他 workload 或接管平台共享对象 |
| `workloadInputs` | 仅 resource-service 的完整 env/secretEnv。Secret 引用仅指向该 profile 的对象/键 |
| `ansibleVariables` | resource_application_ 前缀参数。目标、版本锁、package、对象和values路径由工具派生 |
| `accessSeedFile` | 显式填写旧批准 Access seed 的 config/ 路径，或填 null。首装不需要虚构验收课程/成员 |

渲染输出位于 `resource/`。94-resource-application.yml 通过 `LABWEAVER_RESOURCE_VARS_FILE` 读取其 vars，使用同一 Bundle 的版本锁。Resource 容量/HTTP配置与 package 原样携带，工具不调整费率、计量或既有账。GPU类声明仍由现有角色校验，不代表本工具已验证真实设备。

Resource values 仅启用 resource-service，并保留现有 Resource API egress 所有权。共享 identity proxy、默认拒绝和其他平台对象仍归主 release。未提供 Access seed 时，角色不执行其 SQL，也不导入或清空已有身份。提供旧 seed 时保留原冲突检测。

单文件上限沿用现有渲染器的 1 MiB。整个输入最多 512 个文件、64 MiB。工具不压缩 tar，避免解密后的压缩展开。加密文件上限包含有限的归档开销。超限时拆除无用输入，不要绕过校验。

## 同一目录中的基础设施输入

添加可选 `infrastructure` 字段。该字段包含下列三个键；未使用的秘密文件设为 `null`。

```yaml
infrastructure:
  inventoryFile: config/inventory.yaml
  vaultPasswordFile: null
  identitySecretFile: secrets/identity-bootstrap.yaml
```

`inventoryFile` 使用 Ansible 静态 YAML inventory，顶层只有 `all`，组内使用 `vars`、`hosts`、`children`。不要将 Vault/Jinja 模板作为另一份配置真源。下列片段只展示引用方法，不能直接安装集群：

```yaml
all:
  vars:
    ansible_user: deployment-user
    ansible_ssh_private_key_file:
      file: secrets/deployment-ssh-key
    platform_foundation_bundle:
      file: secrets/foundation-objects.yaml
    platform_buildkit_bundle:
      file: secrets/buildkit-objects.yaml
    platform_buildkit_client_files:
      ca.crt: {file: secrets/build-executor-client/ca.crt}
      tls.crt: {file: secrets/build-executor-client/tls.crt}
      tls.key: {file: secrets/build-executor-client/tls.key}
  children:
    control_plane:
      hosts:
        control-plane:
          ansible_host: 192.0.2.7
```

inventory 的 `file` 引用只接受 `secrets/` 文件。密码类值必须使用 `secret` 引用，派生值使用 Ansible `!unsafe`。SSH 私钥、凭据文件、`*_kubeconfig_path` 和 `*_bundle` 必须使用文件引用。普通远端路径仍按既有角色解释，控制器文件引用不能用作远端 kubeconfig 路径。不能覆盖派生版本锁和身份 locator 摘要，也不能从 inventory 或环境变量关闭 SSH 主机密钥检查。控制器仍须已有获准的 SSH known_hosts；Bundle 不建立持久认证配置。

`vaultPasswordFile` 只供仍需读取既有 Vault 输入的消费者使用。未指定时，执行环境不继承其他 Vault 密码文件；xtask 不查找 `.vault-password`。本 profile 的静态 inventory 使用已经解密的引用值，不再需要第二份 Vault 仓库。

`identitySecretFile` 使用原 identity 角色的 bootstrap YAML。工具派生摘要和本地 locator 标记，角色在受限控制器目录读取 `0600` 文件，并继续使用现有字段校验。无需把该 locator 预先复制到路由器。未指定时，identity playbook 因缺少 locator 明确失败。其旧非 Bundle 调用者继续使用原 root locator。

解密执行提供 `LABWEAVER_BUNDLE_INVENTORY_FILE`、`ANSIBLE_INVENTORY` 和仓库的 `ANSIBLE_CONFIG`。有 Vault 密码时提供 `LABWEAVER_BUNDLE_VAULT_PASSWORD_FILE` 与 `ANSIBLE_VAULT_PASSWORD_FILE`；有身份文件时提供 `LABWEAVER_IDENTITY_SECRET_LOCATOR`。Ansible 本地临时文件及 locator 读取临时目录位于受限运行目录，不依赖控制器主目录。执行环境清除外部日志路径覆盖。所有路径仅在同步消费者运行期间有效。

Cluster network、identity、verify、admin tools、foundation、Harbor、sandbox runtime 和 BuildKit 的文件 lookup 使用派生 `labweaver_versions_lock_file`。应用与 Resource 继续使用自身派生的锁路径。角色中的其他结构、镜像版本约束、命名空间和权限规则仍执行原校验。本入口不将 bootstrap 改成安全升级，也不自动采用共享集群。

## 验证和加密

在操作前准备完整模型及所有引用文件。以下命令均从仓库根目录执行。

1. 检查 age 版本。

   ```sh
   age --version
   ```

2. 验证输入。

   ```sh
   python tools/deployment_bundle.py validate --bundle .private/installation
   ```

   成功后，命令只输出 revision、文件计数和应用对象摘要。验证生成的明文输出随临时目录清理。失败时，命令输出稳定诊断码。

3. 用独立密钥管理位置生成解密身份。

   ```sh
   umask 077
   age-keygen -o .private/operator-age-key.txt
   age-keygen -y .private/operator-age-key.txt > .private/operator-age-recipient.txt
   ```

   身份文件位于 `.private/installation` 和 `.private/bundle-runtime` 之外。组织已有 age 身份时使用现有身份，不重复生成。

4. 加密整个输入目录。

   ```sh
   python tools/deployment_bundle.py seal \
     --bundle .private/installation \
     --recipient "$(cat .private/operator-age-recipient.txt)" \
     --output installation-r7.age
   ```

   可以重复提供 `--recipient`，每个接收者都能解密。命令先验证同一输入快照，再归档和加密。明文归档只存在于受限临时目录。输出通过原子、禁止覆盖的方式发布，权限为 `0600`。

5. 单独保存解密身份。

   只分发 `.age` 文件。不要随文件分发解密身份。将身份及其恢复方式交给获准管理员。

现有 `.age` 文件不会被覆盖。修改配置后创建新 revision 和新文件名。该行为保护已导出的版本，但没有替代管理员 API 所需的并发 revision 条件更新。

## 管理设置导出与导入

已有安装的导出使用管理员版本读取 API。先通过表单或高级 YAML 保存 desired。全新安装使用下一节的离线首装请求。两条路径共用同一个设置类型。

1. 用 `getPlatformConfigurationVersion` 读取 desired 版本。
2. 将 API 返回的完整 JSON 保存为 `.private/server-version.json`，权限为 `0600`。
3. 将验证后的版本写入本次 Bundle 目录。

   ```sh
   python tools/deployment_bundle.py settings-export \
     --version-file .private/server-version.json \
     --output .private/installation/config/platform-configuration.json
   ```

4. 将 `configurationFile` 指向该文件。
5. 将 Bundle revision 设置为该版本的 revision。
6. 用上述 `seal` 命令加密完整目录。

每次准备新版本时使用新的受限目录。已有输出不会被覆盖。修改管理设置时重新取得 Control 版本，不手改其摘要或来源元数据。表单和 YAML 的更新路径仍相同。

导入保留完整设置。源安装的 revision、actor、applied 和执行历史不能覆盖目标安装的事实。

1. 从目标安装读取当前 aggregate revision 和 ETag。
2. 在受限运行目录解密归档并生成更新请求。

   ```sh
   python tools/deployment_bundle.py settings-import \
     --archive installation-r7.age \
     --identity .private/operator-age-key.txt \
     --expected-revision 12 \
     --output .private/import/update.json
   ```

3. 用 `updatePlatformConfiguration` 提交该 JSON、对应 ETag 和稳定幂等键。
4. 遇到 409 或 412 时保留草稿，重新读取并比较目标安装的新配置。
5. 成功后读取目标安装生成的新 desired 版本。

`12` 是命令示例。使用实际目标 revision。命令只生成现有条件更新请求，不发送网络请求、运行部署或写 applied。只有管理员鉴权通过的更新才改变 desired。两个安装的 revision 可以不同，相同设置的 canonical 摘要必须相同。

导入失败不会覆盖已有请求文件。缺 key、错误 key、版本字段或摘要不匹配均失败，运行目录随流程结束清理。普通 stdout 仅包含 revision、计数和摘要，不输出设置或 Secret。

`managedConfigurationSha256` 是管理设置的 canonical 摘要，格式为64个小写十六进制字符。它等于版本 API 的 `configurationSha256`。`applicationObjectsSha256` 是渲染对象的摘要，带 `sha256:` 前缀。执行观察使用管理设置摘要和加密文件摘要，不能把对象摘要当作管理设置摘要。

`seal` 返回实际导出文件的 `encryptedBundleSha256`，并同时返回其源 revision 和管理设置摘要。获准操作员核对它们与当前 desired 一致后，将密文摘要交给管理员应用请求。不要从配置摘要推算密文摘要。导出成功没有批准生产部署。当前 API 尚无获准导出登记或按 desired 查询该登记的入口，执行者仍须核对实际归档。

## 离线准备首装设置

1. 用同一 Rust 模型的默认值准备未保存请求。

   ```sh
   python tools/deployment_bundle.py settings-init --defaults \
     --output .private/installation/config/platform-configuration.json
   ```

2. 如需自定义设置，改用 `--settings-file .private/initial/settings.yaml`，文件只包含完整 configuration 的三个设置组。
3. 将模型设置为 `configurationMode: initial_settings`、`revision: 1`，引用该请求文件。
4. 准备现有应用与 Resource 的完整安装输入，再验证和加密同一目录。
5. 在单独获准安装窗口恢复归档，并使用既有安装入口部署。
6. Control 可用后，由真实管理员提交恢复的零 revision 请求、`If-Match: "rev-0"` 和稳定幂等键。
7. 读取 Control 创建的真实 desired 版本，后续导出使用 `managed_version`。

本地 revision 1 仅表示首装 Bundle 格式中的版本。Control 在提交前仍为未配置，工具不制造 Actor、版本时间或 applied。若目标已经保存新设置，零 revision 更新会冲突，不能把首装当作升级或重置。工具也不会初始化身份、写费率、清空账本或代替用户批准密文/目的地。

## 解密、恢复和使用

1. 在新目录中验证加密文件。

   ```sh
   python tools/deployment_bundle.py --runtime .private/restored-runtime exec \
     --archive installation-r7.age \
     --identity .private/operator-age-key.txt
   ```

   命令完成 age 解密和完整性校验后才解包。随后执行同一模型校验和部署渲染。正确身份能恢复相同源文件，原明文目录可以不存在。缺少身份、错误身份或篡改密文会失败。

2. 让已审查的同步消费者读取运行目录。

   ```sh
   python tools/deployment_bundle.py exec \
     --archive installation-r7.age \
     --identity .private/operator-age-key.txt \
     -- python -c 'import os; from pathlib import Path; assert Path(os.environ["LABWEAVER_APPLICATION_VARS_FILE"]).is_file()'
   ```

   消费者获得 `LW_BUNDLE_ROOT`、`LW_BUNDLE_RENDERED_ROOT` 和 `LABWEAVER_APPLICATION_VARS_FILE`。有 Resource 时还获得 `LABWEAVER_RESOURCE_VARS_FILE`。设置按模式提供 `LABWEAVER_PLATFORM_CONFIGURATION_VERSION_FILE` 或 `LABWEAVER_PLATFORM_CONFIGURATION_UPDATE_FILE`，分别指向保存版本或未保存首装请求。此示例只验证本地输入，不执行部署。

3. 等待消费者退出。

   工具关闭消费者进程组，再清理明文目录。消费者必须同步完成，不得将凭据交给运行目录外的后台任务。消费者 stdout 和 stderr 不进入普通日志。失败时只输出 `LW_BUNDLE_COMMAND_FAILED`。本地排障先独立检查非秘密配置和依赖，避免通过打印凭据恢复日志。

应用部署会改变集群资源和运行实例。只在单独获准的部署窗口将上述消费者换为正式 Ansible 入口。本次功能测试没有执行该操作。平台配置的运行重启、资源影响和费用说明仍须由后续管理员接口返回。

## 失败恢复和密钥轮换

| 诊断 | 下一步 |
| --- | --- |
| `LW_BUNDLE_AGE_REQUIRED` | 安装固定的 age CLI，再重试 |
| `LW_BUNDLE_IDENTITY_REQUIRED` | 提供单独持有的身份文件，并检查 `0600` 权限 |
| `LW_BUNDLE_AGE_FAILED` | 核对接收者和身份，或重新获取未损坏的加密文件。不要打印身份内容 |
| `LW_BUNDLE_DOCUMENT_INVALID` | 检查 YAML 语法、重复键和别名 |
| `LW_BUNDLE_CONFIGURATION_TOOL_REQUIRED` | 构建同一版本的 xtask，并提供正确二进制路径 |
| `LW_BUNDLE_CONFIGURATION_INVALID` | 取得完整管理版本，核对 capability、执行位置和字段 |
| `LW_BUNDLE_CONFIGURATION_DIGEST_MISMATCH` | 重新导出管理版本，不手改快照或摘要 |
| `LW_BUNDLE_CONFIGURATION_REVISION_MISMATCH` | 将 Bundle revision 与引用的管理版本保持一致 |
| `LW_BUNDLE_INITIAL_REVISION_INVALID` | 首装请求 expectedRevision 使用0，首装 Bundle revision 使用1。已有安装继续使用条件更新 |
| `LW_BUNDLE_PROFILE_OWNERSHIP_CONFLICT` | 保留独立 Resource release，移除其他 workload 的启用覆盖 |
| `LW_BUNDLE_WORKLOAD_INPUT_MISSING` | 补齐启用服务的完整环境输入 |
| `LW_BUNDLE_WORKLOAD_OBJECT_MISSING` | 使用包含该服务对象的 profile，不引用归档外的配置 |
| `LW_BUNDLE_INVENTORY_MISMATCH` | 补齐引用文件或删除未引用文件 |
| `LW_BUNDLE_PERMISSIONS_INVALID` | 检查当前用户归属和目录/文件权限 |
| `LW_BUNDLE_OUTPUT_EXISTS` | 使用新 revision 和新输出文件名 |
| `LW_BUNDLE_COMMAND_FAILED` | 修复消费者依赖后用同一加密文件重试。该诊断不代表集群副作用已回滚 |
| `LW_BUNDLE_INTERRUPTED` | 核对消费者的外部结果后重试。工具已关闭进程组并清理本地明文 |

成功、消费者失败、解密失败、校验失败和 SIGINT/SIGTERM 都会清理本次明文目录。SIGKILL 或断电无法执行清理。此时，管理员先确认没有消费者仍在运行，再删除准确的残留 `bundle-*` 运行目录。不要删除整个 `.private` 目录或正在使用的运行目录。工具不会自行恢复未确认的外部部署结果。

轮换服务凭据时，只修改模型引用的 Secret 文件，再导出新 Bundle。多个对象引用同一文件时，它们一起使用新值。Secret 不进入 Git，明文 Kubernetes Secret 的 base64 也不能用于公开分发。

轮换 age 身份时，使用旧身份在受限执行目录打开旧文件。用新接收者调用 `seal`，源目录取 `LW_BUNDLE_ROOT`。新文件验证成功后，再按组织的密钥恢复规则退役旧身份。本工具不删除或轮换用户身份文件。

## 准备隔离恢复验收

`tools/prepare_bundle_acceptance.py` 在 Bundle `exec` 内生成验收清单，不连接集群。它只输出配置 JSON，Secret、SSH key、Vault 密码和 age 身份均不进入清单。输出文件使用 `0600`，已存在的文件不会被覆盖。

真实集群测试复用执行端已经获准的专用 namespace。由唯一验证 owner 设置 `LW_TEST_NAMESPACE`，核对该 namespace、上下文、已有负载和安全设置。后续轮次不另建 namespace，也不删除整个 namespace。

用新解密目录恢复实际归档，再生成清单：

```sh
python tools/deployment_bundle.py exec \
  --archive installation-r7.age --identity .private/operator-age-key.txt \
  -- python tools/prepare_bundle_acceptance.py \
  --namespace "$LW_TEST_NAMESPACE" --output .private/acceptance/restore.yaml
```

输出路径须在 Bundle 临时目录之外。namespace 必须以 `lw-bundle-203-` 开头，并与应用及 Resource 目标不同。工具从同一版本锁的 `test_images.busybox` 选择固定摘要镜像；缺少该镜像时明确失败，不另给镜像默认值。

使用合成凭据验证入口时，运行测试 fixture 命令。它使用 Control 同源默认值和未保存的零 revision 首装请求，新建合成 age 身份、完整归档和隔离恢复目录，完成真实 age 加解密及同一 Rust 类型校验，输出清单后清理所有临时秘密：

```sh
python tests/ansible/prepare_bundle_acceptance_fixture.py \
  --namespace "$LW_TEST_NAMESPACE" --output .private/acceptance/synthetic.yaml
```

默认输出4个对象：ResourceQuota、NetworkPolicy、ConfigMap 和 Job，不包含 Namespace。仅在另行获准准备新 namespace 时使用 `--create-namespace`；该选项只生成第5个 Namespace 对象，不执行创建。复用模式要求既有 namespace 已启用获准的 restricted Pod Security。Pod 默认拒绝网络、禁用服务账户 token、非 root、drop ALL、只读根文件系统。没有 PVC、hostPath、hostNetwork、GPU、Secret 或集群角色。Job 读取已恢复且经过类型校验的配置，核对挂载后的实际字节摘要。成功输出 `BUNDLE_RESTORE_SMOKE_OK`。

执行端需要 Linux amd64 节点、锁定 BusyBox 镜像的拉取能力和已有 CNI NetworkPolicy 支持。Pod 请求50m CPU、64Mi 内存、16Mi 临时存储，上限250m CPU、128Mi 内存、64Mi 临时存储；本 fixture quota 限制2个 Pod、1个 Job、2个 ConfigMap、0个 Secret/PVC，CPU/内存上限分别为1核和512Mi。ConfigMap 配额包含控制器自动发布的 kube-root-ca.crt 和一个 fixture 配置对象。其他测试超出此 fixture 预算时，由唯一验证 owner 核对实际资源需求；不要把 quota 拒绝记成配置恢复失败。

Kubernetes 的 [root CA publisher](https://github.com/kubernetes/kubernetes/blob/v1.35.0/pkg/controller/certificates/rootcacertpublisher/publisher.go) 会为 namespace 创建 kube-root-ca.crt；禁用 Pod token 自动挂载不会关闭该控制器。该测试配额预留这一系统对象，避免创建顺序导致配置对象被拒绝。

每轮由唯一验证 owner 执行：

1. 核对 namespace 和对象归属。quota 与 deny-all policy 只由该 owner 在已获准范围内建立或更新，不强制接管已有对象。
2. 对重跑先读取上次 Job 的终态、日志和错误。确认 `job/bundle-restore`、`configmap/bundle-configuration` 的标签同时为 `labweaver.io/issue=203` 和 `app.kubernetes.io/name=labweaver-bundle-acceptance`，再回收这两个测试对象。等待本次 Job 的 Pod 回收后继续。
3. 保留 namespace、kube-root-ca.crt 和已获准的网络/安全策略。不要对整份清单执行 `kubectl delete -f`，也不要默认删除 namespace。
4. 用 `kubectl create` 创建本轮 ConfigMap/Job，避免覆盖活跃测试或将旧 Succeeded 当成本轮结果。读取 Job 终态与日志标记，记录实际调度、拉取、Pod Security 和回收结果。
5. 结束后按归属回收本轮 Job/配置对象，等 Pod 完成回收后，再回收本 fixture 自建的 `resourcequota/bundle-acceptance`，避免0 Secret/PVC等临时限制阻挡其他已获准测试。不能删除其他 quota、deny-all policy 或 namespace。输出 metadata 的 `cleanupObjects` 仅列这三个 fixture 对象，本工具不会执行远端回收。

此入口验证控制器恢复、配置挂载和隔离 Job。六服务空环境安装、真实身份签发、数据库迁移、入口证书、镜像 Registry 与浏览器流程仍需要专门的隔离安装窗口和获准依赖。现有 foundation/identity/Harbor 角色含固定 namespace 或共享对象，不能直接套用到该测试 namespace。不要将此 Job 成功记录为完整安装或用户旅程通过。

## 当前未完成范围

管理版本、离线首装请求、Resource profile 和静态基础设施 inventory/凭据引用已进入同一加密目录。旧 PKI/NATS 签发 workspace、Harbor/公网入口的远端 root locator 和实际安装步骤仍需逐项整合；这些入口不会在本地恢复时自动执行。7天/30天/24h/1h/60秒的服务消费者、管理界面的本分支集成与真实后端浏览器流程、实际执行观察器仍需完成。本切片没有落实 #207 的实际运行授权或执行位置。隔离验收清单的 restricted 标签只用于专用测试 namespace，不改变生产 namespace 准入。

剩余范围仍属于 #203 和明确的下游消费者。公共契约已由父线程协调，生成 SDK 从 `crates/contracts` 产生。不要用本地加密或状态测试替代实际授权、执行位置和前后端验收。
