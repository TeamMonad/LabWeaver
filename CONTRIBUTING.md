# Contributing

## Issue 与分支

每个变更必须关联一个 Issue，并明确 Goal、Non-goals、Owner、Reviewer、依赖、失败行为和验收证据。分支使用：

```text
feature/<issue>-<slug>
fix/<issue>-<slug>
test/<issue>-<slug>
docs/<issue>-<slug>
```

## 提交前验证

```sh
cargo xtask check
```

创建或更新 Pull Request 前，必须先执行 `git fetch origin develop` 和 `git rebase origin/develop`，确认分支基于最新 `develop` 且不包含未说明的 merge commit。

不存在的测试或缺失的依赖必须阻断，不得以空成功脚本、旧报告或 Mock 结果替代。

## 评审

- 目标分支 `develop` 至少需要一名 Reviewer；
- 目标分支 `main` 至少需要两名 Reviewer；
- Contract、Schema、Migration、权限、安全策略和评分语义必须由 A 与 B 双人评审；
- 作者不得自行批准并合并核心模块。
