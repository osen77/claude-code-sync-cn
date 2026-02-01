# Claude Code Sync 项目指南

本文档为 claude-code-sync 项目的 AI 助手协作指南，包含架构说明、开发规范和重要注意事项。

## 项目概述

claude-code-sync 是一个 Rust CLI 工具，用于同步 Claude Code 对话历史到 Git/Mercurial 仓库，支持跨设备备份和同步。

- **语言**: Rust 2021 Edition
- **核心功能**: 对话历史同步、冲突解决、跨平台路径处理
- **支持平台**: Windows、macOS、Linux
- **版本控制**: Git (主要) / Mercurial (可选)

## 架构说明

### 模块分层

```
claude-code-sync/
├── src/
│   ├── main.rs              # CLI 入口
│   ├── lib.rs               # 库入口
│   │
│   ├── sync/                # 同步核心模块
│   │   ├── discovery.rs     # 🔑 项目发现和匹配逻辑
│   │   ├── pull.rs          # 拉取远程变更
│   │   ├── push.rs          # 推送本地变更
│   │   ├── init.rs          # 仓库初始化
│   │   ├── state.rs         # 同步状态管理
│   │   └── remote.rs        # 远程操作
│   │
│   ├── parser.rs            # 🔑 JSONL 文件解析
│   ├── scm/                 # 版本控制抽象层
│   │   ├── git.rs           # Git 实现
│   │   ├── hg.rs            # Mercurial 实现
│   │   └── lfs.rs           # Git LFS 支持
│   │
│   ├── merge.rs             # 对话合并逻辑
│   ├── conflict.rs          # 冲突检测
│   ├── interactive_conflict.rs  # 交互式冲突解决
│   │
│   ├── handlers/            # 命令处理器
│   ├── history/             # 操作历史记录
│   ├── undo/                # 撤销操作
│   ├── filter.rs            # 同步过滤器
│   └── config.rs            # 配置管理
│
└── docs/
    └── deployment-and-chinese-fix.md  # 🔑 中文项目名修复文档
```

### 关键数据流

1. **Push 流程**:
   ```
   ~/.claude/projects/ → discovery.rs (扫描)
   → parser.rs (解析 JSONL)
   → filter.rs (过滤)
   → push.rs (复制到 sync repo)
   → scm (提交推送)
   ```

2. **Pull 流程**:
   ```
   remote repo → pull.rs (拉取)
   → discovery.rs (匹配本地项目) ⚠️
   → merge.rs (合并)
   → 复制到 ~/.claude/projects/
   ```

## 核心功能说明

### 1. 项目名匹配 (`sync/discovery.rs`)

**关键函数**: `find_local_project_by_name()`

**匹配策略**:
- **第一遍**: 从目录名编码提取项目名（如 `-Users-mini-Documents-myproject` → `myproject`）
- **第二遍**: 从 JSONL 文件的 `cwd` 字段提取真实项目名（处理中文等非 ASCII 字符）

**重要**:
- 支持跨平台路径（Windows `\` 和 Unix `/`）
- 跳过没有 `cwd` 的文件（如快照文件），继续尝试其他 JSONL

### 2. 路径解析 (`parser.rs`)

**关键函数**: `ConversationSession::project_name()`

**实现**:
```rust
// ✅ 同时支持 Unix 和 Windows 路径分隔符
cwd.split(&['/', '\\'])
    .filter(|s| !s.is_empty())
    .last()
```

**用途**: 从 `cwd` 字段提取项目名，支持跨平台同步

### 3. 多设备模式

**配置**: `use_project_name_only = true`

**效果**:
- 仅使用项目名作为目录名（如 `myproject`）
- 不使用完整路径编码（如 `-Users-mini-Documents-myproject`）
- 支持不同设备上路径不同但项目名相同的场景

## 开发规范

### 代码风格

1. **错误处理**: 使用 `anyhow::Result`，提供清晰的上下文信息
2. **日志**: 使用 `log` crate，分级输出（debug/info/warn/error）
3. **测试**: 单元测试放在模块内 `#[cfg(test)]`，集成测试放在 `tests/`
4. **文档**: 公共 API 必须有文档注释 `///`

### 关键原则

1. **跨平台兼容**
   - ❌ 不要使用 `std::path::Path::file_name()` 处理跨平台路径
   - ✅ 使用 `split(&['/', '\\'])` 同时支持两种分隔符

2. **非 ASCII 字符支持**
   - 中文、日文等项目名会被编码为 `-`
   - 必须从 JSONL 内部 `cwd` 字段获取真实项目名
   - 不能假设目录名等于项目名

3. **文件扫描逻辑**
   - 目录中可能有多个 JSONL 文件（对话、快照、子 agent 等）
   - 遇到无效文件时继续尝试，不要提前 `break`
   - 只有匹配失败时才跳过整个目录

4. **性能考虑**
   - 大量对话文件时避免重复解析
   - 使用增量同步而非全量复制

## 重要注意事项

### ⚠️ 中文项目名支持

**问题**: Windows 推送的中文路径在 Mac/Linux 上无法识别

**原因**:
- Windows 路径: `C:\Users\...\安装环境`
- Mac/Linux 的 `Path::file_name()` 不识别 `\`

**解决**:
- 见 `docs/deployment-and-chinese-fix.md`
- 修改 `parser.rs` 和 `sync/discovery.rs`

### ⚠️ JSONL 文件类型

目录中的 JSONL 文件包括：
- **对话文件**: 包含完整对话历史，有 `cwd` 字段
- **快照文件**: 文件历史快照，无 `cwd` 字段
- **Agent 文件**: 子 agent 对话，可能在子目录中

**扫描策略**: 遍历所有 JSONL 直到找到有效项目名

### ⚠️ 冲突处理

**场景**: 同一对话在不同设备上被修改

**策略**:
- 保留两个版本
- 重命名：`session.jsonl` → `session-conflict-<timestamp>.jsonl`
- 生成冲突报告

## 常用开发命令

### 构建和测试

```bash
# 开发构建
cargo build

# Release 构建
cargo build --release

# 运行单元测试
cargo test

# 运行集成测试
cargo test --test '*'

# 运行特定测试
cargo test test_extract_project_name

# 带日志输出的测试
RUST_LOG=debug cargo test -- --nocapture
```

### 安装和运行

```bash
# 本地安装
cargo install --path . --force

# 运行并查看详细日志
RUST_LOG=debug claude-code-sync pull

# 查看配置
claude-code-sync config --show

# 查看状态
claude-code-sync status
```

### 代码检查

```bash
# Clippy 检查
cargo clippy -- -D warnings

# 格式化
cargo fmt

# 文档生成
cargo doc --open --no-deps
```

## 调试技巧

### 启用详细日志

```bash
# 查看项目匹配过程
RUST_LOG=debug claude-code-sync pull 2>&1 | grep "project_name\|MATCH"

# 查看完整调试信息
RUST_LOG=trace claude-code-sync sync
```

### 常见调试点

1. **项目匹配失败**:
   - 检查 `find_local_project_by_name()` 返回值
   - 确认 JSONL 文件是否包含 `cwd` 字段
   - 验证路径分隔符是否正确处理

2. **JSONL 解析错误**:
   - 检查文件格式是否符合 JSONL 规范
   - 查看 `ConversationEntry` 结构体定义
   - 使用 `jq` 手动验证文件: `cat file.jsonl | jq .`

3. **跨平台问题**:
   - 打印 `cwd` 原始值
   - 验证 `project_name()` 提取结果
   - 检查路径分隔符处理逻辑

## 测试策略

### 单元测试

- `parser.rs`: 测试路径解析（Unix/Windows 路径）
- `sync/discovery.rs`: 测试项目名提取和匹配
- `merge.rs`: 测试对话合并逻辑

### 集成测试

- 创建临时目录和 Git 仓库
- 模拟多设备同步场景
- 验证中文项目名处理

### 测试用例示例

```rust
#[test]
fn test_windows_path_on_unix() {
    let session = create_test_session("C:\\Users\\OSEN\\项目名");
    assert_eq!(session.project_name(), Some("项目名"));
}

#[test]
fn test_skip_snapshot_files() {
    // 创建包含快照文件和对话文件的目录
    // 验证能正确跳过快照文件，找到对话文件
}
```

## 文档维护

- **架构变更**: 更新本文档 "架构说明" 部分
- **新增功能**: 更新 README.md 和 API 文档
- **Bug 修复**: 更新相关文档（如 `docs/deployment-and-chinese-fix.md`）
- **配置变更**: 更新配置示例和说明

## 相关资源

- 原始仓库: https://github.com/perfectra1n/claude-code-sync
- 中文 Fork: https://github.com/osen77/claude-code-sync-cn
- API 文档: https://perfectra1n.github.io/claude-code-sync/
- 问题追踪: GitHub Issues

---

*最后更新: 2026-02-01*
