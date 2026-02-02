#!/bin/bash
# Claude Code Sync - One-click installation script
# Usage: curl -fsSL https://raw.githubusercontent.com/osen77/claude-code-sync-cn/main/install.sh | bash

set -e

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m' # No Color

# Print with color
info() { echo -e "${CYAN}$1${NC}"; }
success() { echo -e "${GREEN}$1${NC}"; }
warn() { echo -e "${YELLOW}$1${NC}"; }
error() { echo -e "${RED}$1${NC}"; }

echo ""
echo -e "${BOLD}${CYAN}🔧 Claude Code Sync 安装程序${NC}"
echo -e "${CYAN}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
echo ""

# Detect OS
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
    Linux*)
        PLATFORM="linux"
        info "检测到系统: Linux ($ARCH)"
        ;;
    Darwin*)
        PLATFORM="macos"
        info "检测到系统: macOS ($ARCH)"
        ;;
    MINGW*|MSYS*|CYGWIN*)
        PLATFORM="windows"
        info "检测到系统: Windows (Git Bash/WSL)"
        ;;
    *)
        error "❌ 不支持的操作系统: $OS"
        exit 1
        ;;
esac

echo ""

# Check for Rust/Cargo
check_rust() {
    if command -v cargo &> /dev/null; then
        CARGO_VERSION=$(cargo --version)
        success "✓ 已安装 Rust: $CARGO_VERSION"
        return 0
    else
        return 1
    fi
}

# Install Rust
install_rust() {
    info "📦 正在安装 Rust..."
    echo ""

    if command -v rustup &> /dev/null; then
        warn "rustup 已存在，尝试更新..."
        rustup update stable
    else
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y

        # Source cargo env
        if [ -f "$HOME/.cargo/env" ]; then
            source "$HOME/.cargo/env"
        fi
    fi

    if check_rust; then
        success "✓ Rust 安装成功"
    else
        error "❌ Rust 安装失败，请手动安装: https://rustup.rs"
        exit 1
    fi
}

# Check Rust installation
if ! check_rust; then
    echo ""
    warn "⚠️  未检测到 Rust/Cargo"
    echo ""
    read -p "是否自动安装 Rust? [Y/n] " -n 1 -r
    echo ""

    if [[ $REPLY =~ ^[Nn]$ ]]; then
        info "请先安装 Rust: https://rustup.rs"
        exit 0
    fi

    install_rust
fi

echo ""

# Install claude-code-sync
info "📦 正在安装 claude-code-sync..."
echo ""

# Try to install from GitHub
REPO_URL="https://github.com/osen77/claude-code-sync-cn.git"

if cargo install --git "$REPO_URL" --force 2>&1; then
    success "✓ claude-code-sync 安装成功"
else
    error "❌ 安装失败"
    echo ""
    info "请尝试手动安装:"
    echo "  git clone $REPO_URL"
    echo "  cd claude-code-sync-cn"
    echo "  cargo install --path ."
    exit 1
fi

echo ""
echo -e "${CYAN}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
success "🎉 安装完成！"
echo ""

# Check if already configured
if claude-code-sync status &> /dev/null; then
    success "✓ 已检测到现有配置"
    echo ""
    read -p "是否重新配置? [y/N] " -n 1 -r
    echo ""

    if [[ ! $REPLY =~ ^[Yy]$ ]]; then
        info "跳过配置。使用 'claude-code-sync setup' 可随时重新配置。"
        exit 0
    fi
fi

echo ""
info "🚀 开始配置..."
echo ""

# Run setup wizard
claude-code-sync setup
