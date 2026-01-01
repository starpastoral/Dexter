#!/bin/bash

INSTALL_DIR="${HOME}/.local/bin"

if [ -f "${INSTALL_DIR}/dexter" ]; then
    echo "🗑️  正在卸载 Dexter..."
    rm "${INSTALL_DIR}/dexter"
    echo "✅ Dexter 已成功卸载"
else
    echo "❌ 未找到已安装的 Dexter"
fi
