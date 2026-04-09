# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

A minimal rewrite of [greetd](https://git.sr.ht/~kennylevinsen/greetd), a Linux login manager daemon. Maintains protocol compatibility with existing greeters (regreet, gtkgreet, tuigreet) while reducing complexity from ~3000 to ~800 lines. Uses synchronous code (no tokio), the `pam` crate, and standard TOML parsing.

## Build & Test Commands

```bash
cargo build                   # Debug build
cargo build --release         # Release build
cargo test                    # All tests
cargo test config::tests      # Config parsing tests only
cargo test ipc::tests         # IPC serialization tests only
```

Tests are limited to modules without system access (config, ipc, error). `main.rs`, `session.rs`, and `terminal.rs` require integration testing on a real system.

## Architecture

```
main.rs      - Event loop, signal handling, socket accept, greeter lifecycle
session.rs   - Session worker process: PAM auth, privilege drop, exec user session
ipc.rs       - Protocol types (Request/Response) with JSON serialization
config.rs    - TOML config parsing with VT selection
terminal.rs  - VT ioctls (activate, get current/next VT, set text mode)
error.rs     - Error types with thiserror
```

### Process Model

```
greetd (root)
    ├── Session Worker (root) - stays root to call pam_close_session()
    │       └── User Session (user) - drops privileges, exec compositor/shell
    └── Greeter (greeter user) - connects via Unix socket
```

### IPC Protocol

Length-prefixed JSON over Unix socket (`/run/greetd-{pid}.sock`). Wire format: `[u32 length (native order)][JSON payload]`. Compatible with `greetd_ipc` crate.

**Requests**: `create_session`, `post_auth_message_response`, `start_session`, `cancel_session`
**Responses**: `success`, `error`, `auth_message`

### Dependencies

- `keyring-protocol` - Local crate for keyring unlock at login
- `peercred-ipc` - Local crate for Unix socket IPC with peer credentials

## Configuration

Located at `/etc/greetd/config.toml`. See DESIGN.md for full config format. Requires PAM services at `/etc/pam.d/greetd` and `/etc/pam.d/greetd-greeter`.
