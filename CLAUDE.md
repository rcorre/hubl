# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

hubl is a Rust-based terminal user interface (TUI) application for searching GitHub code and issues. It provides two main commands:
- `hubl code <query>` - Search code across GitHub repositories
- `hubl issues <query>` - Search GitHub issues

The application uses the GitHub CLI (`gh`) for authentication and makes direct API calls to GitHub's GraphQL and REST APIs.

## Architecture

### Core Components
- **src/main.rs** - Entry point, handles CLI parsing, authentication, and TUI setup
- **src/lib.rs** - Defines CLI structure with clap (code/issues subcommands)
- **src/github/** - GitHub API client modules
  - **mod.rs** - Core Github struct with host/token
  - **code.rs** - Code search functionality and content fetching
  - **issues.rs** - Issue search functionality
- **src/code.rs** - TUI app for code search with preview pane
- **src/issues.rs** - TUI app for issue browsing
- **src/preview.rs** - Code preview and syntax highlighting

### Key Dependencies
- **ratatui** - Terminal UI framework
- **nucleo** - Fuzzy finder for filtering search results
- **reqwest** - HTTP client for GitHub API
- **syntect** - Syntax highlighting in code previews
- **clap** - Command-line argument parsing

### Data Flow
1. Authentication via `gh auth token`
2. GitHub API search requests (GraphQL for code, REST for issues)
3. Results injected into nucleo for fuzzy searching
4. TUI displays filtered results with real-time preview

## Development Commands

### Build and Run
```bash
cargo build
cargo run -- code "your search query"
cargo run -- issues "your search query"
```

### Testing
```bash
cargo test
```

### Linting
```bash
cargo clippy
```

### Configuration
- **clippy.toml** - Clippy linting configuration (currently empty)
- **.cargo/config.toml** - Sets RUST_LOG=trace for debugging

## Authentication Setup
The application requires GitHub CLI authentication:
```bash
gh auth login
```

## Test Data
The **testdata/** directory contains sample JSON responses for development and testing.

## Development Guidelines
- Always ensure rust code is formatted

## Claude Guidance
- you can run cargo build without asking