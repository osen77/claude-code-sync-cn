# claude-code-sync

[![Release](https://github.com/osen77/claude-code-sync-cn/actions/workflows/release-new.yml/badge.svg)](https://github.com/osen77/claude-code-sync-cn/actions/workflows/release-new.yml)

一个用于同步 Claude Code 对话历史的 Rust CLI 工具，支持跨设备备份和自动同步。

![Demo](image1.png)

## 功能特性

- **自动同步** - 启动时自动拉取，退出时自动推送，无需手动操作
- **多设备同步** - 在不同电脑间保持对话历史一致
- **配置同步** - 同步 settings.json、CLAUDE.md 等配置文件，支持跨平台适配
- **智能合并** - 自动合并非冲突的对话变更
- **交互式配置** - 首次运行向导引导完成所有配置
- **自动更新** - 启动时检查新版本，支持一键更新

## 快速开始

### 安装

```bash
# macOS / Linux
curl -fsSL https://raw.githubusercontent.com/osen77/claude-code-sync-cn/main/install.sh | bash

# Windows PowerShell
irm https://raw.githubusercontent.com/osen77/claude-code-sync-cn/main/install.ps1 | iex
```

### 配置

```bash
ccs setup
```

向导会引导你完成所有配置，包括：
1. 选择同步模式
2. 配置远程仓库
3. 设置本地目录
4. 配置自动同步（推荐）

### 使用

配置完成后，使用 `claude-sync` 启动 Claude Code 即可自动同步：

```bash
claude-sync
```

## 文档

📚 **[用户指南](docs/user-guide.md)** - 完整的安装配置、多设备同步、常用命令和故障排查

📚 **[开发者指南](CLAUDE.md)** - 项目架构、开发规范和贡献指南

## 常用命令

| 命令 | 说明 |
|------|------|
| `ccs setup` | 交互式配置向导 |
| `ccs sync` | 双向同步 |
| `ccs automate` | 配置自动同步 |
| `ccs status` | 查看同步状态 |
| `ccs config-sync push` | 推送配置到远程 |
| `ccs config-sync apply <device>` | 应用其他设备配置 |
| `ccs update` | 更新到最新版本 |

更多命令请参阅 [用户指南](docs/user-guide.md)。

## 工作原理

Claude Code 将对话历史存储在 `~/.claude/projects/` 目录下的 JSONL 文件中。

`ccs` 的工作流程：
1. 发现本地 Claude Code 历史中的所有对话文件
2. 复制到 Git 仓库并推送到远程
3. 拉取时，合并远程变更到本地历史
4. 冲突时保留两个版本，生成冲突报告

## 自动同步流程

```
启动时: claude-sync → 自动 pull → 启动 Claude Code
使用中: 检测新项目 → 自动 pull 该项目历史
每轮对话结束: Stop Hook → 自动 push
```

## 配置同步

除了对话历史，还支持跨设备同步 Claude Code 配置：

```bash
# 推送当前配置
ccs config-sync push

# 查看可用设备
ccs config-sync list

# 应用其他设备配置
ccs config-sync apply MacBook-Pro
```

**同步内容**：
- `settings.json` - 权限、模型配置（自动过滤 hooks）
- `CLAUDE.md` - 用户全局指令（支持平台标签过滤）
- `installed_skills.json` - 已安装的 skills 列表

**平台标签**：CLAUDE.md 支持平台特定内容，跨平台应用时自动过滤

```markdown
<!-- platform:macos -->
macOS 专用配置
<!-- end-platform -->
```

详见 [用户指南 - 配置同步](docs/user-guide.md#配置同步)。

## 安全考虑

- 对话历史可能包含敏感信息
- 建议使用私有 Git 仓库
- 推荐使用 SSH 密钥或访问令牌进行认证

## 相关资源

- **中文仓库**: https://github.com/osen77/claude-code-sync-cn
- **上游项目**: https://github.com/perfectra1n/claude-code-sync
- **问题追踪**: https://github.com/osen77/claude-code-sync-cn/issues

## 贡献

欢迎贡献！请 Fork 仓库，创建功能分支，提交 Pull Request。

---

*最后更新: 2026-02-04*
