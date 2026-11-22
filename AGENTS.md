# herdr-pet

## 发布约束

- 每次发版前必须新增 `docs/release-notes/v<version>.md`，面向用户说明本版变更；
  建议按 `## 新增` / `## 修复` / `## 说明` 分节，末尾保留统一的 DMG 签名提示。
  格式参考 `docs/release-notes/v0.1.15.md`。
- 同步更新 `Cargo.toml`、`Cargo.lock` 和 README 中的 tag 示例，提交
  `chore: 发布 v<version>` 后再推送同名 tag。
- CI 会校验说明文件存在；缺失时构建失败，不会创建或更新 GitHub Release。
