---
title: GitHub REST API 云端同步
type: plan
category: 03-功能实现
status: 未确认
priority: 高
created: 2026-03-04
execution_mode: single-agent
tags:
  - spec
  - plan
  - sync
  - github
related: []
---

# GitHub REST API 云端同步设计方案

## 1. 概述

### 1.1 背景

CC Switch 目前支持通过 WebDAV 协议进行跨设备云端同步，将本地 SQLite 数据库和 Skills 目录同步到 WebDAV 服务器（坚果云、Nextcloud、群晖 NAS 等）。但 WebDAV 服务的普及度有限，许多用户没有现成的 WebDAV 服务可用。

GitHub 作为全球最大的代码托管平台，几乎所有开发者都拥有账号，且私有仓库免费、天然支持版本历史。通过 GitHub REST API 实现云端同步，可以大幅降低用户使用门槛。

### 1.2 目标

1. 实现基于 GitHub REST API 的跨设备配置同步
2. 最大程度复用现有 WebDAV 同步的协议逻辑（manifest、SHA256 校验、snapshot 构建）
3. 支持手动上传/下载和自动同步两种模式
4. 提供与 WebDAV 同步并列的前端 UI 配置界面

### 1.3 范围

**包含**：
- GitHub REST API 传输层实现（替代 WebDAV HTTP 层）
- GitHub 同步协议层（复用 WebDAV 同步协议的 manifest 机制）
- 自动同步 worker（复用现有 debounce + merge 机制）
- 后端 Settings 持久化（GitHubSyncSettings）
- 前端配置 UI（GitHub Sync Section）
- Tauri Commands 层

**不包含**：
- 使用 git2/libgit2 进行原生 Git 操作
- GitHub OAuth 登录流程（使用 Personal Access Token）
- 增量同步（沿用全量快照策略）
- 多分支/多仓库管理

## 2. 需求分析

### 2.1 功能需求

#### FR-001: GitHub 连接测试
**描述**：验证用户提供的 Token 和仓库信息是否有效
**输入**：token, repo (owner/repo), branch
**输出**：连接成功/失败 + 错误信息
**业务规则**：
1. 验证 Token 权限（需要 `repo` scope）
2. 验证仓库存在且可访问
3. 验证分支存在

#### FR-002: 上传同步数据
**描述**：将本地 SQLite DB + Skills 打包上传到 GitHub 仓库
**输入**：数据库引用、GitHubSyncSettings
**输出**：上传结果（成功/失败）
**业务规则**：
1. 构建本地快照（复用 `build_local_snapshot`）
2. 按顺序上传：db.sql → skills.zip → manifest.json
3. 每个文件使用 GitHub Contents API 的 PUT 操作
4. 更新文件时需要提供 `sha`（GitHub 文件的 blob SHA）
5. 上传成功后持久化同步状态

#### FR-003: 下载同步数据
**描述**：从 GitHub 仓库下载同步数据并应用到本地
**输入**：GitHubSyncSettings
**输出**：下载结果（成功/失败）
**业务规则**：
1. 先下载 manifest.json 验证协议兼容性
2. 下载并校验 db.sql 和 skills.zip（SHA256）
3. 应用快照（复用 `apply_snapshot`）
4. 更新同步状态

#### FR-004: 查看远端信息
**描述**：获取远端 manifest 信息（设备名、时间、版本等）
**输入**：GitHubSyncSettings
**输出**：远端快照信息 / None

#### FR-005: 自动同步
**描述**：数据库变更时自动上传到 GitHub
**输入**：数据库变更信号
**输出**：自动上传结果
**业务规则**：
1. 复用现有 `notify_db_changed` 机制
2. Debounce 1 秒 + 最长 10 秒合并窗口
3. 下载同步时抑制自动上传

### 2.2 非功能需求

- **安全性**：Token 不明文存储到前端、不写入日志、仓库建议设为 Private
- **性能**：GitHub API 有速率限制（5000 次/小时），需合理控制请求频率
- **可靠性**：网络失败时记录错误状态，不影响应用正常使用
- **文件大小**：GitHub Contents API 单文件上限 100MB，需校验文件大小

## 3. 设计方案

### 3.1 架构设计

```
┌─────────────────────────────────────────────────────────────┐
│                    前端 (React + TS)                          │
│  ┌──────────────────────┐  ┌─────────────────────────────┐  │
│  │ GitHubSyncSection.tsx│  │ WebdavSyncSection.tsx (现有) │  │
│  │    (新增 UI)          │  │                             │  │
│  └──────────┬───────────┘  └──────────────────────────────┘  │
│             │                                                │
│  ┌──────────▼───────────────────────────────────────────┐    │
│  │ settingsApi (lib/api/settings.ts) — 新增 GitHub 方法 │    │
│  └──────────┬───────────────────────────────────────────┘    │
└─────────────┼────────────────────────────────────────────────┘
              │ Tauri IPC
┌─────────────▼────────────────────────────────────────────────┐
│                    后端 (Rust)                                 │
│  ┌────────────────────┐                                      │
│  │ commands/           │                                      │
│  │   github_sync.rs   │ ← 新增 Tauri Commands                │
│  └────────┬───────────┘                                      │
│           │                                                   │
│  ┌────────▼───────────┐  ┌────────────────────────┐          │
│  │ services/           │  │ services/               │          │
│  │   github_sync.rs   │  │   webdav_sync.rs (现有) │          │
│  │   (同步协议层)      │  │                        │          │
│  └────────┬───────────┘  └────────────────────────┘          │
│           │                                                   │
│  ┌────────▼───────────┐  ┌────────────────────────┐          │
│  │ services/           │  │ services/               │          │
│  │   github.rs        │  │   webdav.rs (现有)      │          │
│  │   (HTTP 传输层)     │  │                        │          │
│  └────────────────────┘  └────────────────────────┘          │
│                                                               │
│  ┌──────────────────────────────────────────────┐            │
│  │ services/github_auto_sync.rs                  │            │
│  │ (自动同步 worker，复用 webdav_auto_sync 模式)  │            │
│  └──────────────────────────────────────────────┘            │
└───────────────────────────────────────────────────────────────┘
```

### 3.2 数据模型

#### GitHubSyncSettings（新增，存储在 settings.json）

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubSyncSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub auto_sync: bool,
    /// Personal Access Token (classic 或 fine-grained)
    pub token: String,
    /// 仓库全名，格式 "owner/repo"
    pub repo: String,
    /// 分支名，默认 "main"
    #[serde(default = "default_branch")]
    pub branch: String,
    /// 远端根目录，默认 "cc-switch-sync"
    #[serde(default = "default_remote_root")]
    pub remote_root: String,
    /// 配置档案名，默认 "default"
    #[serde(default = "default_profile")]
    pub profile: String,
    /// 同步状态
    #[serde(default)]
    pub status: GitHubSyncStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GitHubSyncStatus {
    pub last_sync_at: Option<i64>,
    pub last_error: Option<String>,
    pub last_error_source: Option<String>,
    pub last_local_manifest_hash: Option<String>,
    pub last_remote_manifest_hash: Option<String>,
    /// GitHub 文件的 blob SHA（用于更新文件时必须提供）
    pub last_manifest_blob_sha: Option<String>,
}
```

#### 远端文件结构（与 WebDAV 完全一致）

```
{repo}/{remote_root}/v2/{profile}/
├── manifest.json      ← 同步清单（设备名、时间戳、artifact 哈希）
├── db.sql             ← SQLite 数据库导出
└── skills.zip         ← Skills 目录打包
```

### 3.3 接口设计

#### GitHub HTTP 传输层 (`services/github.rs`)

```rust
/// 测试 GitHub 连接（验证 Token + 仓库 + 分支）
pub async fn test_connection(settings: &GitHubSyncSettings) -> Result<(), AppError>;

/// 获取文件内容 + blob SHA。返回 None 表示文件不存在 (404)
pub async fn get_file(
    settings: &GitHubSyncSettings,
    path: &str,
) -> Result<Option<GitHubFile>, AppError>;

/// 创建或更新文件（需要 blob_sha 来更新已存在的文件）
pub async fn put_file(
    settings: &GitHubSyncSettings,
    path: &str,
    content: &[u8],
    blob_sha: Option<&str>,
    commit_message: &str,
) -> Result<String, AppError>;  // 返回新的 blob SHA

/// 仅获取文件的 blob SHA（用于变更检测，不下载内容）
pub async fn head_file_sha(
    settings: &GitHubSyncSettings,
    path: &str,
) -> Result<Option<String>, AppError>;
```

> [!important] GitHub Contents API 关键约束
> - GET 返回 Base64 编码的内容 + `sha` 字段
> - PUT 创建/更新文件时，更新已有文件**必须提供 `sha`**
> - 单文件大小上限 100MB
> - API 速率限制 5000 次/小时（认证用户）
> - 对于大于 1MB 的文件，GET Contents API 不返回内容，需使用 Blobs API

#### GitHub 同步协议层 (`services/github_sync.rs`)

```rust
/// 检查连接
pub async fn check_connection(settings: &GitHubSyncSettings) -> Result<(), AppError>;

/// 上传本地快照到 GitHub
pub async fn upload(
    db: &Database,
    settings: &mut GitHubSyncSettings,
) -> Result<Value, AppError>;

/// 从 GitHub 下载快照并应用到本地
pub async fn download(
    db: &Database,
    settings: &mut GitHubSyncSettings,
) -> Result<Value, AppError>;

/// 获取远端快照信息
pub async fn fetch_remote_info(
    settings: &GitHubSyncSettings,
) -> Result<Option<Value>, AppError>;
```

#### Tauri Commands (`commands/github_sync.rs`)

```rust
#[tauri::command] github_sync_check_connection(settings) -> Result<Value, String>
#[tauri::command] github_sync_upload(state) -> Result<Value, String>
#[tauri::command] github_sync_download(state) -> Result<Value, String>
#[tauri::command] github_sync_fetch_remote_info() -> Result<Option<Value>, String>
#[tauri::command] github_sync_save_settings(settings) -> Result<(), String>
#[tauri::command] github_sync_get_settings() -> Result<Option<GitHubSyncSettings>, String>
```

### 3.4 大文件处理策略

> [!warning] GitHub Contents API 对大文件的限制
> - GET `/repos/{owner}/{repo}/contents/{path}`：文件 > 1MB 时不返回内容
> - 需要改用 Git Blobs API：`GET /repos/{owner}/{repo}/git/blobs/{sha}`

处理策略：
1. **小文件（≤ 1MB）**：直接使用 Contents API 的 GET 获取
2. **大文件（> 1MB，≤ 100MB）**：
   - 上传：仍使用 Contents API 的 PUT（支持到 100MB）
   - 下载：先通过 Contents API 获取 `sha`，再通过 Blobs API 下载
3. **超大文件（> 100MB）**：拒绝同步，提示用户清理数据

```rust
/// 智能获取文件内容：小文件用 Contents API，大文件用 Blobs API
async fn get_file_content(
    settings: &GitHubSyncSettings,
    path: &str,
) -> Result<Option<(Vec<u8>, String)>, AppError> {
    let contents = get_contents_metadata(settings, path).await?;
    match contents {
        None => Ok(None),
        Some(meta) if meta.size <= 1_000_000 => {
            // 小文件：直接解码 Contents API 返回的 base64 content
            let bytes = base64_decode(&meta.content)?;
            Ok(Some((bytes, meta.sha)))
        }
        Some(meta) => {
            // 大文件：通过 Blobs API 下载
            let bytes = get_blob(settings, &meta.sha).await?;
            Ok(Some((bytes, meta.sha)))
        }
    }
}
```

### 3.5 Settings 集成

在现有 `AppSettings` 中添加 `github_sync` 字段，与 `webdav_sync` 并列：

```rust
// settings.rs - AppSettings 新增字段
pub struct AppSettings {
    // ... 现有字段 ...
    pub webdav_sync: Option<WebDavSyncSettings>,  // 现有
    pub github_sync: Option<GitHubSyncSettings>,  // 新增
}
```

> [!important] GitHub 同步与 WebDAV 同步互斥
> 同一时刻只能启用一种云端同步方案。启用 GitHub 同步时自动禁用 WebDAV 同步，反之亦然。这避免了两种同步方式产生冲突。

### 3.6 自动同步复用策略

现有 `webdav_auto_sync.rs` 中的核心机制可以复用：
- `notify_db_changed()` — 数据库变更通知（已在 `database/mod.rs` 中注册 hook）
- `should_trigger_for_table()` — 表过滤逻辑
- Debounce + merge 窗口机制
- `AutoSyncSuppressionGuard` — 下载时抑制上传

实现方式：将自动同步 worker 改为根据当前启用的同步方式（WebDAV / GitHub）路由到不同的上传函数。

### 3.7 前端 UI 设计

在设置页面新增 **GitHub Sync** 区块，与 WebDAV Sync 并列（Tab 或 Radio 切换）：

**配置项**：
- 同步方式选择（WebDAV / GitHub）
- Token 输入框（密码类型，带 "如何获取 Token" 帮助链接）
- 仓库名输入（`owner/repo` 格式，带格式验证）
- 分支名（默认 `main`）
- 远端根目录（默认 `cc-switch-sync`）
- Profile 名（默认 `default`）
- 自动同步开关
- 连接测试按钮
- 手动上传/下载按钮
- 最后同步状态显示

## 4. 执行模式

### 执行模式选择

**推荐模式**：单 Agent

**选择理由**：
- 新增功能边界清晰，与现有代码解耦
- 大量复用现有 WebDAV 同步的协议逻辑，改动可控
- 前后端改动可按顺序串行完成

## 5. 实现步骤

- [ ] **阶段 1：后端传输层**
  - [ ] 1.1 新增 `services/github.rs`：GitHub REST API HTTP 传输层
    - `test_connection()`、`get_file()`、`put_file()`、`head_file_sha()`
    - 大文件处理（Blobs API fallback）
    - 错误处理与国际化错误消息
  - [ ] 1.2 新增 `GitHubSyncSettings` / `GitHubSyncStatus` 到 `settings.rs`
  - [ ] 1.3 新增 settings 管理函数：`get_github_sync_settings()`、`set_github_sync_settings()`、`update_github_sync_status()`

- [ ] **阶段 2：后端同步协议层**
  - [ ] 2.1 新增 `services/github_sync.rs`：同步协议实现
    - 复用 `webdav_sync.rs` 中的 `SyncManifest`、`build_local_snapshot()`、`apply_snapshot()`、`validate_manifest_compat()` 等（提取为共享模块或直接引用）
    - 实现 `upload()`、`download()`、`fetch_remote_info()`、`check_connection()`
  - [ ] 2.2 修改 `services/webdav_auto_sync.rs`（或新增 `github_auto_sync.rs`）：
    - 自动同步路由，根据启用的同步方式调用对应的上传函数
  - [ ] 2.3 互斥逻辑：启用 GitHub 同步时禁用 WebDAV 同步

- [ ] **阶段 3：Tauri Commands 层**
  - [ ] 3.1 新增 `commands/github_sync.rs`：6 个 Tauri Command
  - [ ] 3.2 在 `commands/mod.rs` 中注册新 commands
  - [ ] 3.3 在 `lib.rs` 中注册到 Tauri invoke handler

- [ ] **阶段 4：前端实现**
  - [ ] 4.1 新增 TypeScript 类型定义 (`types.ts`)
  - [ ] 4.2 新增前端 API 封装 (`lib/api/settings.ts` 中添加 GitHub 相关方法)
  - [ ] 4.3 新增 `GitHubSyncSection.tsx` 组件
  - [ ] 4.4 集成到 `SettingsPage.tsx`，实现同步方式切换
  - [ ] 4.5 国际化：添加中/英/日翻译 key

- [ ] **阶段 5：测试与完善**
  - [ ] 5.1 后端单元测试（settings 序列化、manifest 构建、URL 拼接等）
  - [ ] 5.2 前端组件测试
  - [ ] 5.3 错误场景覆盖（Token 无效、仓库不存在、网络超时等）

## 6. 风险和依赖

| 风险 | 影响 | 概率 | 缓解措施 |
|------|------|------|---------|
| GitHub API 速率限制（5000/小时） | 中 | 低 | 自动同步已有 debounce 机制，正常使用不会触发限制 |
| Token 泄露风险 | 高 | 低 | 仅存储在本地 settings.json，前端传输时清空密码，日志脱敏 |
| 大文件（>100MB）无法上传 | 中 | 低 | 校验文件大小，超限时提示用户清理数据 |
| Contents API 大文件下载限制（>1MB） | 中 | 中 | 使用 Blobs API 作为 fallback |
| 仓库为 Public 导致配置暴露 | 高 | 中 | UI 中强提示建议使用 Private 仓库 |
| 并发冲突（多设备同时上传） | 中 | 低 | GitHub PUT 需要 `sha`，天然实现了乐观锁 |

**依赖**：
- `reqwest`（已有依赖）— HTTP 请求
- `base64`（已有依赖）— GitHub API 返回 Base64 编码内容
- `sha2`（已有依赖）— SHA256 校验
- 无需新增 crate 依赖

## 文档关联

- 实现总结: [[summary|实现总结]] (待创建)
- 测试计划: [[test-plan|测试计划]] (待创建，由 spec-tester 创建)
- 相关模块: `src-tauri/src/services/webdav.rs` — 现有 WebDAV 传输层（参考实现）
- 相关模块: `src-tauri/src/services/webdav_sync.rs` — 现有同步协议层（复用逻辑）
- 相关模块: `src-tauri/src/services/webdav_auto_sync.rs` — 现有自动同步（复用机制）
