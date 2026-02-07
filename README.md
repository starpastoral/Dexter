# Dexter

> **The Retro-Futurist AI Command Copilot**

Dexter is a terminal-based AI assistant that routes natural-language intent to specialized CLI tools (such as `f2`, `ffmpeg`, `pandoc`, `qpdf`, `ocrmypdf`, and `yt-dlp`), builds commands, and enforces a confirmation-first execution flow.

## Features

- **Intelligent Routing**: A router model selects the most suitable plugin for each request.
- **Multi-Provider LLM Support**:
  - OpenAI
  - Anthropic
  - OpenRouter
  - Moonshot
  - Gemini
  - DeepSeek
  - Groq
  - Baseten
  - Ollama (local)
  - OpenAI-compatible endpoints
  - Anthropic-compatible endpoints
- **Provider + Model Fallback**:
  - Multiple providers can be configured and stored at the same time.
  - Providers can be configured but kept disabled at runtime.
  - Multiple active models can be selected and ordered for fallback.
- **Guided Setup + Runtime Settings TUI**:
  - First-run guided setup.
  - Re-open settings anytime via the footer `[SETTINGS]` button.
- **Safety First**:
  - High-risk commands are blocked.
  - No execution without explicit confirmation.
  - Working-directory context is included for safer command generation.
- **PDF Power Tools**:
  - `qpdf` for PDF structural checks, linearization, page selection/merge, and encryption/decryption workflows.
  - `ocrmypdf` for searchable OCR, language-aware OCR, and scan cleanup workflows.
- **Retro TUI (ratatui)**:
  - Themed terminal UI.
  - Narrow-terminal adaptive layout (compact footer/buttons and dynamic setup table widths).

## Getting Started

### Prerequisites

- Rust (latest stable)
- `f2` in `$PATH`
- `ffmpeg` in `$PATH`
- `yt-dlp` in `$PATH`
- `pandoc` in `$PATH` (optional; required for document conversions)
- `qpdf` in `$PATH` (optional; required for PDF structural workflows)
- `ocrmypdf` in `$PATH` (optional; required for OCR/searchable PDF workflows)

### Quick Install

1. Clone repository:
```bash
git clone https://github.com/your-username/dexter.git
cd dexter
```

2. Run installer:
```bash
chmod +x install.sh
./install.sh
```

3. Start:
```bash
dexter
```

### Manual Build

```bash
cargo build --release
mkdir -p ~/.local/bin
cp target/release/dexter ~/.local/bin/
```

Ensure `~/.local/bin` is in your `PATH`.

## Setup Wizard Flow

Configuration file: `~/.config/dexter/config.toml`

Dexter setup now follows this linear flow:

1. **Providers Toggle**
2. **Provider Config**
3. **Models Toggle**
4. **All Models Confirmation / Fallback Order**
5. **Enter = Save & Leave**, **Esc = Back to Step 1**

Key behavior:

- `Space` toggles provider/model selection.
- `Enter` on Step 1 starts the guided setup sequence.
- Step 3 includes a `Select All` row.
- Step 4 supports reordering via `U/K` (up) and `D/J` (down).

## Usage

Launch Dexter and describe your task in natural language.

Examples:

- **Batch renaming**:
  - "Rename all .jpeg files to .jpg and add a `vacation_` prefix."
- **Media processing**:
  - "Convert input.mp4 to a high-quality GIF, crop it to square, and optimize."
- **Media downloading**:
  - "Download this YouTube video as mp3 and save it to `./music`."

## Architecture

- `dexter_core`: LLM routing, model/provider fallback, safety logic.
- `dexter_plugins`: Plugin system and tool adapters.
- `dexter_tui`: Terminal UI and setup/settings flow.

## Notes

- Router JSON parsing is now tolerant of partial/invalid `clarify` payloads from models and falls back to normal plugin routing when clarify data is incomplete.

## Roadmap

- Add ImageMagick plugin support.
- Improve history and pinned proposals.
- Expand keyboard shortcuts and power-user workflows.

## Contributing

Contributions are welcome. Please keep safety constraints intact and follow existing project style.

## License

MIT
