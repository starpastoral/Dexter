# Dexter

> **The Retro-Futurist AI Command Copilot**

Dexter is a terminal-based AI assistant designed to democratize powerful command-line utilities. It acts as a "Co-pilot" for your shell, routing your natural language intent to specialized tools (like `f2` and `ffmpeg`), constructing complex commands, and ensuring safe execution through a strict verification process.

## Features

-   **Intelligent Routing**: Dexter's Router Agent analyzes your request and selects the best tool for the job.
-   **Safety First**:
    -   **Sandboxed Execution**: Dangerous commands (`rm`, `mv /`, `dd`) are hard-blocked.
    -   **Confirmation Loop**: No command runs without your explicit approval.
    -   **Context Awareness**: Understands your current directory structure to make smarter decisions.
-   **Plugin Architecture**:
    -   **F2**: Powerful batch renaming.
    -   **FFmpeg**: Complex media conversion and processing with AI-generated previews and logic validation.
-   **Premium Retro-Futurist UI**: A stunning TUI inspired by CRT terminals, built with `ratatui`.
    -   **Multiple Themes**: Choose between the classic **Amber Retro**, a modern **Light Mode**, or let **Auto** decide based on your system.

## Getting Started

### Prerequisites

-   **Rust** (latest stable)
-   **FFmpeg** (must be in your `$PATH`)
-   **F2** (must be in your `$PATH`)

### Quick Install (Recommended)

1.  Clone the repository:
    ```bash
    git clone https://github.com/your-username/dexter.git
    cd dexter
    ```

2.  Run the automated installation script:
    ```bash
    chmod +x install.sh
    ./install.sh
    ```

3.  After installation, simply run:
    ```bash
    dexter
    ```

### Manual Build

If you prefer to build from source:

1.  Compile the release binary:
    ```bash
    cargo build --release
    ```

2.  Install to your local bin:
    ```bash
    mkdir -p ~/.local/bin
    cp target/release/dexter ~/.local/bin/
    ```

3.  Ensure `~/.local/bin` is in your `PATH`.

## Configuration & Guided Setup

Dexter features a **Guided Setup Wizard** that launches automatically on your first run. It will assist you with:

1.  **API Key Configuration**: Securely enter your Gemini, DeepSeek.
2.  **Dynamic Model Discovery**: Automatically fetches and lets you select the latest available models from your providers.
3.  **Environment Check**: Verifies that required plugins (`f2`, `ffmpeg`) are correctly installed.

Configuration is persisted in `~/.config/dexter/config.toml`.

## Usage

Simply launch Dexter and type your intent in natural language.

**Examples:**

*   **Batch Renaming**:
    > "Rename all .jpeg files to .jpg and add a 'vacation_' prefix."
    
*   **Media Processing**:
    > "Convert input.mp4 to a high-quality GIF, crop it to square, and optimize."
    
    *Dexter will validate the FFmpeg flags and provide a dry-run preview before execution.*

## Architecture

Dexter is built as a modular Rust workspace:

-   `dexter_core`: The core logic (LLM communication, routing, and safety).
-   `dexter_plugins`: Extensible plugin system for external CLI tools.
-   `dexter_tui`: The high-fidelity terminal interface.

## Roadmap

### Plugins
- Add ImageMagick support
- Add yt-dlp support
- Add Pandoc support

### Features
- Add Groq support
- Add Baseten support
- Add providers fallback
- Improve setup flow, add provider validation
- Proposal can be regenerated or edited
- Keyboard shortcuts support
- Proposal history and pinned proposals
- LLM cache hit

## Contributing

Contributions are welcome! Please ensure new plugins implement the necessary safety traits and match the Amber-monochrome aesthetic.

## License

MIT License
