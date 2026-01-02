#!/bin/bash

INSTALL_DIR="${HOME}/.local/bin"

if [ -f "${INSTALL_DIR}/dexter" ]; then
    echo "🗑️  Uninstalling Dexter..."
    rm "${INSTALL_DIR}/dexter"
    echo "✅ Dexter Uninstalled Successfully!"
else
    echo "❌ NO Dexter Installed"
fi
