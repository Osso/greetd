# greetd Rewrite Design Document

## Overview

A minimal rewrite of [greetd](https://git.sr.ht/~kennylevinsen/greetd), a login manager daemon for Linux. This implementation maintains protocol compatibility with existing greeters (regreet, gtkgreet, tuigreet, etc.) while significantly reducing complexity.

## Goals

1. **Simplicity** - Reduce LOC from ~3000 to ~800 while maintaining core functionality
2. **Compatibility** - Same IPC protocol, works with existing greeters
3. **Testability** - High test coverage on business logic
4. **Readability** - Sync code over async, fewer abstractions

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                        greetd daemon                         │
│                         (PID 1-ish)                          │
├─────────────────────────────────────────────────────────────┤
│  main.rs         │  Event loop, signal handling, socket     │
│  config.rs       │  TOML config parsing                     │
│  ipc.rs          │  Protocol serialization (JSON)           │
│  session.rs      │  PAM auth, session spawning              │
│  terminal.rs     │  VT ioctls                               │
│  error.rs        │  Error types                             │
└─────────────────────────────────────────────────────────────┘
           │
           │ Unix socket (JSON over length-prefixed frames)
           ▼
┌─────────────────────────────────────────────────────────────┐
│                    Greeter (e.g., regreet)                   │
│                  Runs as unprivileged user                   │
└─────────────────────────────────────────────────────────────┘
```

## Process Model

```
greetd (root)
    │
    ├── fork() → Session Worker (root)
    │                │
    │                ├── PAM authentication
    │                ├── open_session()
    │                │
    │                └── fork() → User Session (user)
    │                                 │
    │                                 ├── drop privileges
    │                                 └── exec(compositor/shell)
    │
    └── Greeter process (greeter user)
```

The session worker stays root to call `pam_close_session()` when the user session exits. This is required for proper session teardown (unmounting home dirs, notifying logind, etc.).

## IPC Protocol

Length-prefixed JSON messages over Unix socket. Compatible with `greetd_ipc` crate.

### Requests (Greeter → Daemon)

| Type | Fields | Description |
|------|--------|-------------|
| `create_session` | `username` | Start auth flow for user |
| `post_auth_message_response` | `response?` | Answer PAM prompt |
| `start_session` | `cmd`, `env` | Launch authenticated session |
| `cancel_session` | - | Abort current auth flow |

### Responses (Daemon → Greeter)

| Type | Fields | Description |
|------|--------|-------------|
| `success` | - | Operation succeeded |
| `error` | `error_type`, `description` | Operation failed |
| `auth_message` | `auth_message_type`, `auth_message` | PAM prompt |

### Wire Format

```
┌──────────────┬─────────────────────────────────┐
│ length (u32) │ JSON payload (UTF-8)            │
│ native order │ {"type":"...", ...}             │
└──────────────┴─────────────────────────────────┘
```

## Authentication Flow

```
Greeter                    greetd                     PAM
   │                          │                        │
   │ create_session(user)     │                        │
   │─────────────────────────>│                        │
   │                          │ pam_authenticate()     │
   │                          │───────────────────────>│
   │                          │<───────────────────────│
   │                          │   conv: "Password:"    │
   │  auth_message(secret)    │                        │
   │<─────────────────────────│                        │
   │                          │                        │
   │ post_auth_response(pw)   │                        │
   │─────────────────────────>│                        │
   │                          │ conv_response(pw)      │
   │                          │───────────────────────>│
   │                          │<───────────────────────│
   │                          │   PAM_SUCCESS          │
   │       success            │                        │
   │<─────────────────────────│                        │
   │                          │                        │
   │ start_session(["sway"])  │                        │
   │─────────────────────────>│                        │
   │       success            │                        │
   │<─────────────────────────│                        │
   │                          │                        │
   │ (greeter exits)          │ (starts user session)  │
```

## Greeter Lifecycle

1. **Startup**: greetd spawns greeter as `greeter` user
2. **Running**: Greeter connects to socket, handles login UI
3. **Login**: User authenticates, greeter calls `start_session`
4. **Exit**: Greeter should exit after successful `start_session`
5. **Timeout**: If greeter doesn't exit within 5s → SIGTERM, 10s → SIGKILL
6. **Restart**: After user session ends, greetd spawns new greeter

## Signal Handling

| Signal | Action |
|--------|--------|
| `SIGCHLD` | Ignored (poll via `waitpid`) |
| `SIGTERM` | Graceful shutdown |
| `SIGINT` | Graceful shutdown |

Graceful shutdown:
1. Cancel pending sessions
2. SIGTERM to current greeter/session
3. Wait 500ms
4. SIGKILL if still alive
5. Remove socket file

## Configuration

```toml
[terminal]
vt = 1              # VT number, "next", "current", or "none"
switch = true       # Switch to VT on start

[general]
source_profile = true           # Source /etc/profile
runfile = "/run/greetd.run"     # First-run marker
service = "greetd"              # PAM service for users

[default_session]
command = "cage -s -- regreet"  # Greeter command
user = "greeter"                # User to run greeter as
service = "greetd-greeter"      # PAM service for greeter

[initial_session]               # Optional: auto-login on first boot
command = "sway"
user = "alice"
```

## Comparison with Original

| Aspect | Original | Rewrite |
|--------|----------|---------|
| Lines of code | ~3000 | ~800 |
| Async runtime | tokio | sync/blocking |
| PAM wrapper | Custom (pam-sys) | pam crate |
| Config parser | Custom (inish) | toml + serde |
| IPC crate | Separate (greetd_ipc) | Inline |
| Test coverage | Minimal | 95%+ (testable code) |

### Simplifications

1. **No tokio** - A login manager handles one session at a time; async adds complexity without benefit
2. **Higher-level PAM** - The `pam` crate handles conversation callbacks cleanly
3. **Standard TOML** - No custom parser needed
4. **Inline IPC** - Protocol is simple enough to not warrant a separate crate

### Not Implemented

- **Password scrambling** - Secure memory wiping of credentials
- **Full TTY setup** - Connecting stdin/stdout/stderr to TTY, taking controlling terminal
- **Multi-step PAM** - Complex auth flows (TOTP after password)
- **Custom conversation** - Only password-based auth currently

## Security Considerations

1. **Memory locking** - `mlockall()` prevents credentials from being swapped
2. **Privilege separation** - Greeter runs as unprivileged user
3. **Socket permissions** - Socket owned by greeter user
4. **No credential storage** - Passwords passed directly to PAM, not stored

## File Locations

| Path | Purpose |
|------|---------|
| `/etc/greetd/config.toml` | Configuration |
| `/run/greetd-{pid}.sock` | IPC socket |
| `/run/greetd.run` | First-run marker |
| `/etc/pam.d/greetd` | PAM config for users |
| `/etc/pam.d/greetd-greeter` | PAM config for greeter |

## Testing

```bash
cargo test                    # Run all tests
cargo test config::tests      # Config parsing tests
cargo test ipc::tests         # IPC serialization tests
cargo test error::tests       # Error type tests
```

Coverage is limited to modules that don't require system access (PAM, TTY, root). The `main.rs`, `session.rs`, and `terminal.rs` modules require integration testing on a real system.
