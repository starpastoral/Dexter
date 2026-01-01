#!/bin/bash
set -e

echo "🚀 安装 Dexter..."

# 构建 release 版本
echo "📦 正在编译..."
cargo build --release

# 确定安装目录
INSTALL_DIR="${HOME}/.local/bin"

# 创建安装目录（如果不存在）
mkdir -p "${INSTALL_DIR}"

# 复制二进制文件
echo "📋 正在安装到 ${INSTALL_DIR}/dexter..."
cp target/release/dexter "${INSTALL_DIR}/dexter"
chmod +x "${INSTALL_DIR}/dexter"

echo ""
echo "✅ Dexter 安装成功！"
echo ""

# 检查 PATH 配置
if [[ ":$PATH:" != *":${INSTALL_DIR}:"* ]]; then
    echo "⚠️  注意：${INSTALL_DIR} 不在你的 PATH 中"
    echo ""
    echo "请将以下内容添加到你的 shell 配置文件中："
    
    # 检测 shell 类型
    if [ -n "$ZSH_VERSION" ]; then
        SHELL_CONFIG="~/.zshrc"
    elif [ -n "$BASH_VERSION" ]; then
        SHELL_CONFIG="~/.bashrc"
    else
        SHELL_CONFIG="~/.profile"
    fi
    
    echo ""
    echo "  echo 'export PATH=\"\$HOME/.local/bin:\$PATH\"' >> ${SHELL_CONFIG}"
    echo "  source ${SHELL_CONFIG}"
    echo ""
else
    echo "🎉 你现在可以在任何地方运行 'dexter' 命令了！"
    echo ""
fi

echo "使用方法："
echo "  dexter          # 启动 Dexter AI 助手"
echo ""
