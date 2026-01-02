#!/bin/bash
set -e

echo "Installing Dexter..."

# 构建 release 版本
echo "Compiling..."
cargo build --release

# 确定安装目录
INSTALL_DIR="${HOME}/.local/bin"

# 创建安装目录（如果不存在）
mkdir -p "${INSTALL_DIR}"

# 复制二进制文件
echo "Installing to ${INSTALL_DIR}/dexter..."
cp target/release/dexter "${INSTALL_DIR}/dexter"
chmod +x "${INSTALL_DIR}/dexter"

echo ""
echo "Dexter Installed Successfully!"
echo ""

# 检查 PATH 配置
if [[ ":$PATH:" != *":${INSTALL_DIR}:"* ]]; then
    echo "⚠️ NOTE: ${INSTALL_DIR} Is Not In Your PATH"
    echo ""
    echo "Please Add The Following To Your Shell Config File:"
    
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
    echo "🎉  You Can Now Run DEXTER Anywhere!"
    echo ""
fi

echo "Type 'dexter' To Start!"
echo ""
