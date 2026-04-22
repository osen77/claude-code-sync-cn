# 项目问题记录

## 2026-04-22: Pull 无法匹配含连字符的项目名 (v0.3.8)

### 问题描述
- `use_project_name_only = true` 模式下，pull 无法将远程会话合并到本地含连字符的项目目录（如 `ux-workspace`）
- 用户在电脑 B 上有两个 `ux-workspace` 目录（不同路径），远程同步的会话全部被跳过

### 根本原因
1. **`extract_project_name()` 对含 `-` 的项目名提取错误**：用 `rsplit('-')` 取最后一段，`-Users-abc-ux-workspace` 提取出 `workspace` 而非 `ux-workspace`。路径编码的 `-` 和项目名自带的 `-` 无法区分
2. **`find_local_project_by_name()` 逻辑缺陷**：第一轮 dir name 匹配失败后，第二轮 JSONL cwd 匹配找到第一个就立即返回，不检查是否有多个匹配（歧义）；且多目录时直接返回 None

### 解决方案
重写 `find_local_project_by_name()` (`src/sync/discovery.rs`)：
- 两轮匹配（dir name + JSONL cwd）**始终都跑**，收集全部结果后合并去重
- cwd 匹配优先（精确），dir name 匹配补充
- 多匹配时 `log::warn` 输出歧义信息并返回 None
- 抽取 `get_project_name_from_dir()` 辅助函数，`find_colliding_projects()` 也复用以正确检测含连字符项目名的碰撞

### 影响范围
- Pull 会话匹配、Pull memory 同步匹配、碰撞检测
- Push 不受影响（使用 `session.project_name()` 从 cwd 正确提取）

### 预防措施
- 新增 3 个测试用例覆盖含连字符项目名的匹配和歧义场景
- memory 同步部分（`pull.rs:632`）已有注释说明不使用 `extract_project_name()`

## 2026-02-19: CI 构建失败导致 Release 无二进制文件

### 问题描述
- 使用 `release.sh` 推送 tag `v0.1.11` 后，GitHub Actions 构建失败
- 用户执行 `claude-code-sync update` 时返回 404 错误
- 原因：release 页面存在但没有二进制文件

### 根本原因
GitHub Actions workflow 配置问题：
- `strategy` 缺少 `fail-fast: false` 配置
- 默认 `fail-fast: true` 导致一个平台（如 `x86_64-unknown-linux-musl`）构建失败时，取消所有其他平台的构建
- 最终 release 创建成功但没有任何可下载的二进制文件

### 解决方案
1. **修改 `.github/workflows/release-new.yml`**
   ```yaml
   strategy:
     fail-fast: false  # 添加此行
     matrix:
       include:
         ...
   ```

2. **删除失败的 tag 并重新发布**
   ```bash
   git tag -d v0.1.11
   git push origin :v0.1.11
   # 修改版本号并重新发布
   ```

### 影响范围
- v0.1.11 release 失败（已删除）
- v0.1.12 已修复并重新发布

### 预防措施
- CI 配置中始终使用 `fail-fast: false` 确保各平台独立构建
- 可考虑将 musl 构建设为 `continue-on-error: true`（如果该平台不是必需的）

---

## 2026-02-19: Session 管理功能增强

### 新增功能
1. **标题过滤增强** - 过滤系统标签：
   - `<task-notification>`
   - `<local-command-caveat>`
   - `<command-name>`
   - `<local-command-stdout>`

2. **序号显示** - session list 显示 `[1]` `[2]` 序号前缀

3. **搜索功能** - 在用户消息中搜索关键词
   - 第一个选项为 "Search sessions..."
   - 显示匹配片段预览（围绕关键词截取上下文）
   - 支持大小写不敏感搜索

### 修改文件
- `src/parser.rs` - 增强 `is_system_content()` 和新增 `extract_user_text()`
- `src/handlers/session.rs` - 新增搜索相关函数和交互逻辑

---

## 开发规范

### 问题记录要求
遇到以下情况必须记录到本文件：
1. **构建/部署失败** - 记录原因、影响、解决方案
2. **功能变更** - 记录新增/修改的功能、影响范围
3. **Bug 修复** - 记录问题现象、根本原因、修复方法
4. **性能优化** - 记录优化前后对比、影响范围
5. **依赖更新** - 记录版本变更、兼容性问题

### 记录格式
```markdown
## YYYY-MM-DD: 问题简述

### 问题描述
- 现象
- 影响

### 根本原因
- 技术细节

### 解决方案
- 具体步骤

### 影响范围
- 版本号
- 相关模块

### 预防措施
- 后续改进
```
