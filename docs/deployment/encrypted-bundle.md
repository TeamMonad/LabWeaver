# 加密应用 Bundle

本入口实现 #203 的应用配置与加密目录切片。[管理员配置 API](../contracts/platform-configuration.md) 已提供 revision 和应用事实管理。它与 Bundle 的完整接线、运行权限消费者和基础设施安装尚未完成。不要据此关闭 #203。

## 配置归属

`tools/deployment_bundle.py` 读取一个受限目录中的 `config/bundle.yaml`。该文件引用同一目录中的服务配置、Secret、版本清单和运维说明。服务配置继续使用现有服务类型。`deploy/config/platform-bundle-manifest.json` 定义应用允许的对象和键，不保存安装值。

目录中的文件是输入。运行目录中的 ConfigMap/Secret、Helm values 和 Ansible vars 是派生输出。不要编辑派生输出，也不要将其用作下一次导出的输入。多个对象需要同一凭据时，引用同一个 Secret 文件。每个服务的 NATS 身份仍使用各自的 credential。

`93-platform-application.yml` 在载入 Bundle 派生的 vars 后读取 `platform_application_versions_lock_file`。该路径指向 Bundle 的版本锁。已有部署入口未指定该字段时，仍读取仓库的 `deploy/versions.lock.yml`。这保留现有调用者，不能证明它们已经采用加密入口。

本切片不改变服务鉴权、集群权限、安装操作或账务。基础设施 inventory、Resource 独立部署 profile 和旧凭据签发入口尚未完成迁入。服务启动时仍执行现有配置类型的业务校验。本工具校验目录、引用、应用对象集合、版本入口和现有 NATS ACK 权限。

## 前提

- 使用支持 POSIX 文件权限的 Linux 或 macOS 控制器。Windows 使用受限的 Linux 控制器或 WSL。
- 安装 Python 和 `tools/requirements-local-dev.txt` 中固定的 PyYAML。
- 安装官方 [age v1.3.2](https://github.com/FiloSottile/age/releases/tag/v1.3.2)。本仓库的 CI 校验 Linux amd64 归档 SHA-256 `cbe24006683f8eb669266162894b9a522a1af52f2665fbc63a4bb032ed26ac10`。
- 单独保管 age 解密身份文件。不要将它放入 Bundle 或 Bundle 运行目录。
- 用已有签发流程准备服务凭据。不要从日志、聊天记录或已部署 Secret 反向恢复私钥。

## 目录和模型

```text
.private/installation/
  config/bundle.yaml
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
| `target.namespace` | Kubernetes DNS label |
| `target.releaseName` | Helm release 名称，使用 Kubernetes DNS label |
| `versions.lockFile` | `versions/` 下的文件。沿用 `schema_version: 1` 的部署版本锁 |
| `versions.packageFile` | `versions/` 下的构建 package JSON。镜像必须使用唯一组件名和 SHA-256 引用 |
| `docs.operationsFile` | `docs/` 下的非空运维说明 |
| `application.configMaps` | 对象名、键和 `config/` 下文件路径的映射 |
| `application.secrets` | 对象名、键和 `secrets/` 下文件路径的映射 |
| `application.helmValues` | 非秘密 Helm 输入。镜像和 `deploymentIdentity` 由工具派生 |
| `application.ansibleVariables` | 应用角色参数。只接受 `platform_application_` 前缀 |

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

单文件上限沿用现有渲染器的 1 MiB。整个输入最多 512 个文件、64 MiB。工具不压缩 tar，避免解密后的压缩展开。加密文件上限包含有限的归档开销。超限时拆除无用输入，不要绕过校验。

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

   消费者获得 `LW_BUNDLE_ROOT`、`LW_BUNDLE_RENDERED_ROOT` 和 `LABWEAVER_APPLICATION_VARS_FILE`。后者接入既有应用角色的 vars 入口。此示例只验证本地输入，不执行部署。

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
| `LW_BUNDLE_INVENTORY_MISMATCH` | 补齐引用文件或删除未引用文件 |
| `LW_BUNDLE_PERMISSIONS_INVALID` | 检查当前用户归属和目录/文件权限 |
| `LW_BUNDLE_OUTPUT_EXISTS` | 使用新 revision 和新输出文件名 |
| `LW_BUNDLE_COMMAND_FAILED` | 修复消费者依赖后用同一加密文件重试。该诊断不代表集群副作用已回滚 |
| `LW_BUNDLE_INTERRUPTED` | 核对消费者的外部结果后重试。工具已关闭进程组并清理本地明文 |

成功、消费者失败、解密失败、校验失败和 SIGINT/SIGTERM 都会清理本次明文目录。SIGKILL 或断电无法执行清理。此时，管理员先确认没有消费者仍在运行，再删除准确的残留 `bundle-*` 运行目录。不要删除整个 `.private` 目录或正在使用的运行目录。工具不会自行恢复未确认的外部部署结果。

轮换服务凭据时，只修改模型引用的 Secret 文件，再导出新 Bundle。多个对象引用同一文件时，它们一起使用新值。Secret 不进入 Git，明文 Kubernetes Secret 的 base64 也不能用于公开分发。

轮换 age 身份时，使用旧身份在受限执行目录打开旧文件。用新接收者调用 `seal`，源目录取 `LW_BUNDLE_ROOT`。新文件验证成功后，再按组织的密钥恢复规则退役旧身份。本工具不删除或轮换用户身份文件。

## 当前未完成范围

管理员 JSON/YAML API、desired/applied/失败阶段和策略契约已由同一 Control 配置模块提供，见[配置接口说明](../contracts/platform-configuration.md)。管理设置与本入口的整体导入/导出接线尚未完成。7天/30天/24h/1h/60秒的服务消费者、Grok 表单和实际执行观察器仍未交付。本切片没有放宽 namespace Pod Security，也没有落实 #207 的实际运行授权或执行位置。

剩余范围仍属于 #203 和明确的下游消费者。公共契约已由父线程协调，生成 SDK 从 `crates/contracts` 产生。不要用本地加密或状态测试替代实际授权、执行位置和前后端验收。
