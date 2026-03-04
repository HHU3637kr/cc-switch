---
title: GitHub REST API 云端同步 — 实现总结
type: summary
category: 03-功能实现
status: 已完成
created: 2026-03-04
tags:
  - spec
  - summary
  - sync
  - github
related:
  - plan.md
---

# GitHub REST API 云端同步 — 实现总结

## 1. 完成状态

所有 plan.md 中规划的阶段均已完成：

- [x] **阶段 1：后端传输层**
  - [x] 1.1 `services/github.rs` — GitHub REST API HTTP 传输层
  - [x] 1.2 `GitHubSyncSettings` / `GitHubSyncStatus` 添加到 `settings.rs`
  - [x] 1.3 settings 管理函数 (get/set/update)
- [x] **阶段 2：后端同步协议层**
  - [x] 2.1 `services/github_sync.rs` — 同步协议实现
  - [x] 2.2 修改 `webdav_auto_sync.rs` 支持 GitHub 路由
  - [x] 2.3 互斥逻辑（GitHub/WebDAV 二选一）
- [x] **阶段 3：Tauri Commands 层**
  - [x] 3.1 `commands/github_sync.rs` — 5 个 Tauri Command
  - [x] 3.2 在 `commands/mod.rs` 注册
  - [x] 3.3 在 `lib.rs` 注册到 invoke handler
- [x] **阶段 4：前端实现**
  - [x] 4.1 TypeScript 类型定义
  - [x] 4.2 前端 API 封装
  - [x] 4.3 `GitHubSyncSection.tsx` 组件
  - [x] 4.4 集成到 `SettingsPage.tsx`
  - [x] 4.5 国际化（中/英/日）

## 2. 新增/修改文件清单

### 新增文件

| 文件 | 说明 |
|------|------|
| `src-tauri/src/services/github.rs` | GitHub REST API HTTP 传输层（test_connection, get_file, put_file, head_file_sha） |
| `src-tauri/src/services/github_sync.rs` | GitHub 同步协议层（upload, download, fetch_remote_info, run_with_sync_lock） |
| `src-tauri/src/commands/github_sync.rs` | Tauri Commands（github_test_connection, github_sync_upload, github_sync_download, github_sync_save_settings, github_sync_fetch_remote_info） |
| `src/components/settings/GitHubSyncSection.tsx` | GitHub 同步前端配置 UI 组件 |

### 修改文件

| 文件 | 修改内容 |
|------|---------|
| `src-tauri/src/settings.rs` | 新增 `GitHubSyncSettings`、`GitHubSyncStatus` 结构体，`github_sync` 字段，管理函数，前端 token 脱敏 |
| `src-tauri/src/services/mod.rs` | 注册 `github` 和 `github_sync` 模块 |
| `src-tauri/src/commands/mod.rs` | 注册 `github_sync` 模块并导出 |
| `src-tauri/src/lib.rs` | 在 invoke_handler 注册 5 个 GitHub sync 命令 |
| `src-tauri/src/services/webdav_auto_sync.rs` | 自动同步 worker 增加 GitHub 路由逻辑 |
| `src-tauri/src/services/webdav_sync.rs` | 将 `archive` 模块、helper 函数、常量可见性改为 `pub(crate)` |
| `src-tauri/src/services/webdav_sync/archive.rs` | 将 `SkillsBackup` 及相关函数可见性改为 `pub(crate)` |
| `src-tauri/src/commands/webdav_sync.rs` | 保存 WebDAV 配置时添加互斥逻辑（禁用 GitHub 同步） |
| `src/types.ts` | 新增 `GitHubSyncSettings`、`GitHubSyncStatus` 接口，Settings 增加 `githubSync` 字段 |
| `src/lib/api/settings.ts` | 新增 5 个 GitHub sync API 函数 |
| `src/lib/schemas/settings.ts` | 新增 `githubSync` Zod schema |
| `src/components/settings/SettingsPage.tsx` | 导入并渲染 `GitHubSyncSection` |
| `src/hooks/useSettings.ts` | 通用保存 payload 排除 `githubSync` |
| `src/App.tsx` | 添加 `github-sync-status-updated` 事件监听 |
| `src/i18n/locales/zh.json` | GitHub 同步中文翻译 |
| `src/i18n/locales/en.json` | GitHub 同步英文翻译 |
| `src/i18n/locales/ja.json` | GitHub 同步日文翻译 |

## 3. 关键设计决策

### 3.1 复用 WebDAV 同步基础设施

- **Manifest 协议**：沿用 `cc-switch-webdav-sync` v2 格式，保持跨同步方式兼容
- **Archive 模块**：直接复用 `webdav_sync::archive` 的 skills zip/restore 逻辑，通过 `pub(crate)` 可见性共享
- **自动同步**：复用现有 `webdav_auto_sync.rs` 的 debounce + merge 机制，添加 GitHub 路由分支

### 3.2 互斥同步策略

- 同一时刻只能启用 WebDAV 或 GitHub 其中一种同步
- 启用 GitHub 同步时自动禁用 WebDAV，反之亦然
- 在 `github_sync_save_settings` 和 `webdav_sync_save_settings` 命令中双向实现

### 3.3 大文件处理

- Contents API GET：文件 > 1MB 时不返回内容，自动 fallback 到 Blobs API
- Contents API PUT：支持到 100MB
- 超过 100MB 的文件拒绝上传，返回错误提示

### 3.4 Token 安全

- 前端获取配置时 token 被清空（`get_settings_for_frontend`）
- 保存时支持 `tokenTouched` 参数，未修改则保留后端已有 token
- 日志输出中 token 脱敏

### 3.5 并发控制

- 独立的 `tokio::sync::Mutex` 锁（与 WebDAV 锁分离）
- 手动上传/下载和自动同步共享同一把锁

## 4. 与 plan.md 的偏差

| 偏差 | 说明 |
|------|------|
| 未新增独立的 `github_auto_sync.rs` | plan 提到可以新增独立文件或修改现有文件，实际选择修改 `webdav_auto_sync.rs` 添加路由逻辑，更简洁 |
| Tauri Commands 为 5 个而非 6 个 | plan 列出 `github_sync_get_settings` 命令，实际通过通用的 `get_settings` 命令获取（含 githubSync 字段），无需单独命令 |
| `emit_auto_sync_status_updated` 增加 `event_name` 参数 | 为区分 WebDAV 和 GitHub 事件，将事件名参数化 |

## 5. 待验证事项

- 编译验证：当前开发环境缺少 MSVC Build Tools（`link.exe` 未找到），需在完整构建环境中验证编译
- 端到端测试：需实际 GitHub Token 和私有仓库进行功能测试
- 大文件场景：需准备 > 1MB 的测试数据验证 Blobs API fallback

## 文档关联

- 设计方案: [[plan|设计方案]]
- 测试计划: [[test-plan|测试计划]] (待创建，由 spec-tester 创建)
