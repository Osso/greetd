mod config;
mod error;
mod ipc;
mod session;
mod terminal;

use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use std::fs;

use nix::sys::signal::{sigaction, kill, SaFlags, SigAction, SigHandler, SigSet, Signal};
use nix::unistd::{chown, getpid};

use config::{Config, VtSelection};
use error::Error;
use ipc::{Request, Response, AuthMessageType};
use session::{Session, SessionClass};
use terminal::{Terminal, TerminalMode};

static TERMINATE: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_terminate(_: libc::c_int) {
    TERMINATE.store(true, Ordering::SeqCst);
}

fn should_terminate() -> bool {
    TERMINATE.load(Ordering::SeqCst)
}

fn main() {
    if let Err(e) = run() {
        eprintln!("greetd: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Error> {
    let config_path = std::env::args().nth(1)
        .unwrap_or_else(|| "/etc/greetd/config.toml".into());

    let config = Config::load(&config_path)?;

    // Lock memory to prevent secrets from being swapped
    unsafe {
        libc::mlockall(libc::MCL_CURRENT | libc::MCL_FUTURE);
    }

    // Setup signal handlers
    setup_signal_handlers()?;

    // Determine terminal mode
    let term_mode = get_terminal_mode(&config)?;

    // Get greeter user info
    let greeter_user = nix::unistd::User::from_name(&config.greeter_user)?
        .ok_or_else(|| format!("greeter user not found: {}", config.greeter_user))?;

    // Create socket
    let sock_path = format!("/run/greetd-{}.sock", getpid().as_raw());
    let _ = fs::remove_file(&sock_path);
    let listener = UnixListener::bind(&sock_path)?;
    chown(sock_path.as_str(), Some(greeter_user.uid), Some(greeter_user.gid))?;

    // Check if PAM service exists
    let pam_service = find_pam_service(&config.service)?;
    let greeter_pam_service = find_pam_service(&config.greeter_service)
        .unwrap_or_else(|_| pam_service.clone());

    // Handle initial session if configured and first run
    let runfile = Path::new(&config.runfile);
    if !runfile.exists() {
        if let Some(initial) = &config.initial_session {
            eprintln!("greetd: starting initial session for {}", initial.user);
            start_session_direct(
                &pam_service,
                SessionClass::User,
                &initial.user,
                vec![initial.command.clone()],
                &term_mode,
                config.source_profile,
                &sock_path,
            )?;
        }
    }

    // Create runfile
    fs::write(&config.runfile, "")?;

    // Start greeter
    eprintln!("greetd: starting greeter: {} (pam={}, user={})", config.greeter_command, greeter_pam_service, config.greeter_user);
    let mut greeter = start_session_direct(
        &greeter_pam_service,
        SessionClass::Greeter,
        &config.greeter_user,
        vec![config.greeter_command.clone()],
        &term_mode,
        config.source_profile,
        &sock_path,
    )?;
    eprintln!("greetd: greeter started, pid={}", greeter.pid());

    // Main loop state
    let mut pending_session: Option<Session> = None;
    let mut pending_since: Option<Instant> = None;
    let mut sent_term: bool = false;

    listener.set_nonblocking(true)?;

    while !should_terminate() {
        // Check for dead children
        while let Some((pid, _code)) = session::reap_children() {
            if pid == greeter.pid() {
                // Greeter exited
                if let Some(session) = pending_session.take() {
                    // Start pending user session
                    greeter = session;
                    pending_since = None;
                    sent_term = false;
                } else {
                    // Restart greeter
                    greeter = start_session_direct(
                        &greeter_pam_service,
                        SessionClass::Greeter,
                        &config.greeter_user,
                        vec![config.greeter_command.clone()],
                        &term_mode,
                        config.source_profile,
                        &sock_path,
                    )?;
                }
            }
        }

        // Handle greeter timeout when session is pending
        if pending_session.is_some() {
            let elapsed = pending_since.get_or_insert_with(Instant::now).elapsed();

            if elapsed > Duration::from_secs(10) {
                // Out of patience - SIGKILL
                eprintln!("greetd: greeter not responding, sending SIGKILL");
                let _ = kill(greeter.pid(), Signal::SIGKILL);
            } else if elapsed > Duration::from_secs(5) && !sent_term {
                // Gentle nudge - SIGTERM
                eprintln!("greetd: greeter not exiting, sending SIGTERM");
                let _ = kill(greeter.pid(), Signal::SIGTERM);
                sent_term = true;
            }
        }

        // Accept connections
        if let Ok((stream, _)) = listener.accept() {
            stream.set_nonblocking(false)?;
            if let Err(e) = handle_client(
                stream,
                &pam_service,
                &term_mode,
                config.source_profile,
                &sock_path,
                &mut pending_session,
                &mut pending_since,
                &mut sent_term,
            ) {
                eprintln!("greetd: client error: {e}");
            }
        }

        std::thread::sleep(Duration::from_millis(100));
    }

    // Graceful shutdown
    eprintln!("greetd: shutting down");

    // Cancel any pending session
    if let Some(mut session) = pending_session.take() {
        let _ = session.cancel();
    }

    // Terminate the current greeter/session
    let _ = kill(greeter.pid(), Signal::SIGTERM);

    // Wait briefly for graceful exit
    std::thread::sleep(Duration::from_millis(500));

    // Force kill if still running
    let _ = kill(greeter.pid(), Signal::SIGKILL);

    // Clean up socket
    let _ = fs::remove_file(&sock_path);

    Ok(())
}

fn setup_signal_handlers() -> Result<(), Error> {
    // Ignore SIGCHLD - we poll for children
    let ignore = SigAction::new(SigHandler::SigIgn, SaFlags::empty(), SigSet::empty());
    unsafe { sigaction(Signal::SIGCHLD, &ignore)? };

    // Handle SIGTERM and SIGINT for graceful shutdown
    let terminate = SigAction::new(
        SigHandler::Handler(handle_terminate),
        SaFlags::empty(),
        SigSet::empty(),
    );
    unsafe {
        sigaction(Signal::SIGTERM, &terminate)?;
        sigaction(Signal::SIGINT, &terminate)?;
    }

    Ok(())
}

fn get_terminal_mode(config: &Config) -> Result<TerminalMode, Error> {
    match &config.vt {
        VtSelection::None => Ok(TerminalMode::Stdin),
        VtSelection::Current => {
            let term = Terminal::open("/dev/tty0")?;
            let vt = term.current_vt()?;
            Ok(TerminalMode::Vt {
                path: format!("/dev/tty{vt}"),
                vt,
                switch: false,
            })
        }
        VtSelection::Next => {
            let term = Terminal::open("/dev/tty0")?;
            let vt = term.next_vt()?;
            Ok(TerminalMode::Vt {
                path: format!("/dev/tty{vt}"),
                vt,
                switch: config.switch,
            })
        }
        VtSelection::Specific(vt) => Ok(TerminalMode::Vt {
            path: format!("/dev/tty{vt}"),
            vt: *vt,
            switch: config.switch,
        }),
    }
}

fn find_pam_service(name: &str) -> Result<String, Error> {
    for dir in &["/etc/pam.d", "/usr/lib/pam.d"] {
        if Path::new(&format!("{dir}/{name}")).exists() {
            return Ok(name.to_string());
        }
    }
    Err(format!("PAM service not found: {name}").into())
}

fn start_session_direct(
    service: &str,
    class: SessionClass,
    user: &str,
    cmd: Vec<String>,
    term_mode: &TerminalMode,
    source_profile: bool,
    greetd_sock: &str,
) -> Result<Session, Error> {
    let mut session = Session::spawn()?;
    session.initiate(service, class, user, false, term_mode, source_profile, greetd_sock)?;

    // Skip auth prompts for unauthenticated sessions
    while session.get_auth_prompt()?.is_some() {
        session.respond(None)?;
    }

    session.start(cmd, vec![])?;
    Ok(session)
}

#[allow(clippy::too_many_arguments)]
fn handle_client(
    mut stream: UnixStream,
    pam_service: &str,
    term_mode: &TerminalMode,
    source_profile: bool,
    greetd_sock: &str,
    pending_session: &mut Option<Session>,
    pending_since: &mut Option<Instant>,
    sent_term: &mut bool,
) -> Result<(), Error> {
    let mut session: Option<Session> = None;

    loop {
        let req = match Request::read_from(&mut stream)? {
            Some(r) => r,
            None => break, // EOF
        };

        let resp = match req {
            Request::CreateSession { username } => {
                // Cancel any existing session
                if let Some(mut s) = session.take() {
                    let _ = s.cancel();
                }

                let mut s = Session::spawn()?;
                s.initiate(
                    pam_service,
                    SessionClass::User,
                    &username,
                    true,
                    term_mode,
                    source_profile,
                    greetd_sock,
                )?;

                let resp = match s.get_auth_prompt()? {
                    Some((prompt, echo)) => Response::AuthMessage {
                        auth_message_type: if echo {
                            AuthMessageType::Visible
                        } else {
                            AuthMessageType::Secret
                        },
                        auth_message: prompt,
                    },
                    None => Response::Success,
                };

                session = Some(s);
                resp
            }

            Request::PostAuthMessageResponse { response } => {
                match &mut session {
                    Some(s) => {
                        s.respond(response)?;
                        match s.get_auth_prompt()? {
                            Some((prompt, echo)) => Response::AuthMessage {
                                auth_message_type: if echo {
                                    AuthMessageType::Visible
                                } else {
                                    AuthMessageType::Secret
                                },
                                auth_message: prompt,
                            },
                            None => Response::Success,
                        }
                    }
                    None => Response::error("no session active"),
                }
            }

            Request::StartSession { cmd, env } => {
                match session.take() {
                    Some(mut s) => {
                        match s.start(cmd.clone(), env) {
                            Ok(_) => {
                                eprintln!("greetd: session started: {}", cmd.join(" "));
                                *pending_session = Some(s);
                                *pending_since = Some(Instant::now());
                                *sent_term = false;
                                Response::Success
                            }
                            Err(e) => Response::error(e.to_string()),
                        }
                    }
                    None => Response::error("no session active"),
                }
            }

            Request::CancelSession => {
                if let Some(mut s) = session.take() {
                    let _ = s.cancel();
                }
                Response::Success
            }
        };

        resp.write_to(&mut stream)?;
    }

    // Clean up on disconnect
    if let Some(mut s) = session.take() {
        let _ = s.cancel();
    }

    Ok(())
}
