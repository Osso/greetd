mod config;
mod error;
mod ipc;
mod session;
mod terminal;

use std::fs;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, kill, sigaction};
use nix::unistd::{chown, getpid};

use config::{Config, InitialSession, VtSelection};
use error::Error;
use ipc::{AuthMessageType, Request, Response};
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
    if let Err(error) = run() {
        eprintln!("greetd: {error}");
        std::process::exit(1);
    }
}

struct Runtime {
    pam_service: String,
    greeter_pam_service: String,
    greeter_command: String,
    greeter_user: String,
    term_mode: TerminalMode,
    source_profile: bool,
    sock_path: String,
    runfile: String,
    initial_session: Option<RuntimeInitialSession>,
}

struct RuntimeInitialSession {
    user: String,
    command: String,
}

#[derive(Default)]
struct PendingState {
    session: Option<Session>,
    since: Option<Instant>,
    sent_term: bool,
}

struct ClientContext<'a> {
    runtime: &'a Runtime,
    pending_state: &'a mut PendingState,
}

fn run() -> Result<(), Error> {
    let config = load_config()?;
    lock_memory();
    setup_signal_handlers()?;

    let runtime = Runtime::from_config(config)?;
    let listener = create_listener(&runtime.sock_path, &runtime.greeter_user)?;

    start_initial_session_if_needed(&runtime)?;
    create_runfile(&runtime.runfile)?;

    eprintln!(
        "greetd: starting greeter: {} (pam={}, user={})",
        runtime.greeter_command, runtime.greeter_pam_service, runtime.greeter_user
    );
    let mut greeter = start_greeter(&runtime)?;
    eprintln!("greetd: greeter started, pid={}", greeter.pid());

    listener.set_nonblocking(true)?;
    let mut pending_state = PendingState::default();
    event_loop(&listener, &runtime, &mut greeter, &mut pending_state)?;
    shutdown(&runtime.sock_path, &mut greeter, &mut pending_state);
    Ok(())
}

impl Runtime {
    fn from_config(config: Config) -> Result<Self, Error> {
        let term_mode = get_terminal_mode(&config)?;
        let pam_service = find_pam_service(&config.service)?;
        let greeter_pam_service =
            find_pam_service(&config.greeter_service).unwrap_or_else(|_| pam_service.clone());
        let initial_session = config.initial_session.map(RuntimeInitialSession::from);

        Ok(Self {
            pam_service,
            greeter_pam_service,
            greeter_command: config.greeter_command,
            greeter_user: config.greeter_user,
            term_mode,
            source_profile: config.source_profile,
            sock_path: format!("/run/greetd-{}.sock", getpid().as_raw()),
            runfile: config.runfile,
            initial_session,
        })
    }
}

impl From<InitialSession> for RuntimeInitialSession {
    fn from(initial: InitialSession) -> Self {
        Self {
            user: initial.user,
            command: initial.command,
        }
    }
}

impl PendingState {
    fn queue(&mut self, session: Session) {
        self.session = Some(session);
        self.since = Some(Instant::now());
        self.sent_term = false;
    }

    fn promote(&mut self) -> Option<Session> {
        let session = self.session.take();
        self.since = None;
        self.sent_term = false;
        session
    }

    fn enforce_timeout(&mut self, greeter: &Session) {
        if self.session.is_none() {
            return;
        }

        let elapsed = self.since.get_or_insert_with(Instant::now).elapsed();
        if elapsed > Duration::from_secs(10) {
            eprintln!("greetd: greeter not responding, sending SIGKILL");
            let _ = kill(greeter.pid(), Signal::SIGKILL);
            return;
        }

        if elapsed > Duration::from_secs(5) && !self.sent_term {
            eprintln!("greetd: greeter not exiting, sending SIGTERM");
            let _ = kill(greeter.pid(), Signal::SIGTERM);
            self.sent_term = true;
        }
    }

    fn cancel_pending(&mut self) {
        if let Some(mut session) = self.session.take() {
            let _ = session.cancel();
        }
        self.since = None;
        self.sent_term = false;
    }
}

fn load_config() -> Result<Config, Error> {
    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/etc/greetd/config.toml".into());
    Config::load(&config_path)
}

fn lock_memory() {
    unsafe {
        libc::mlockall(libc::MCL_CURRENT | libc::MCL_FUTURE);
    }
}

fn create_listener(sock_path: &str, greeter_user: &str) -> Result<UnixListener, Error> {
    let greeter_user = nix::unistd::User::from_name(greeter_user)?
        .ok_or_else(|| format!("greeter user not found: {greeter_user}"))?;

    let _ = fs::remove_file(sock_path);
    let listener = UnixListener::bind(sock_path)?;
    chown(sock_path, Some(greeter_user.uid), Some(greeter_user.gid))?;
    Ok(listener)
}

fn start_initial_session_if_needed(runtime: &Runtime) -> Result<(), Error> {
    if Path::new(&runtime.runfile).exists() {
        return Ok(());
    }

    let Some(initial) = &runtime.initial_session else {
        return Ok(());
    };

    eprintln!("greetd: starting initial session for {}", initial.user);
    let request = DirectSessionRequest {
        service: &runtime.pam_service,
        class: SessionClass::User,
        user: &initial.user,
        cmd: vec![initial.command.clone()],
    };
    let _ = start_session_direct(runtime, request)?;
    Ok(())
}

fn create_runfile(runfile: &str) -> Result<(), Error> {
    fs::write(runfile, "")?;
    Ok(())
}

fn event_loop(
    listener: &UnixListener,
    runtime: &Runtime,
    greeter: &mut Session,
    pending_state: &mut PendingState,
) -> Result<(), Error> {
    while !should_terminate() {
        reap_greeter_exits(runtime, greeter, pending_state)?;
        pending_state.enforce_timeout(greeter);
        handle_next_client(listener, runtime, pending_state)?;
        std::thread::sleep(Duration::from_millis(100));
    }

    Ok(())
}

fn reap_greeter_exits(
    runtime: &Runtime,
    greeter: &mut Session,
    pending_state: &mut PendingState,
) -> Result<(), Error> {
    while let Some((pid, _code)) = session::reap_children() {
        if pid != greeter.pid() {
            continue;
        }

        if let Some(session) = pending_state.promote() {
            *greeter = session;
        } else {
            *greeter = start_greeter(runtime)?;
        }
    }

    Ok(())
}

fn handle_next_client(
    listener: &UnixListener,
    runtime: &Runtime,
    pending_state: &mut PendingState,
) -> Result<(), Error> {
    let Ok((stream, _)) = listener.accept() else {
        return Ok(());
    };

    stream.set_nonblocking(false)?;
    let mut context = ClientContext {
        runtime,
        pending_state,
    };
    if let Err(error) = handle_client(stream, &mut context) {
        eprintln!("greetd: client error: {error}");
    }

    Ok(())
}

fn shutdown(sock_path: &str, greeter: &mut Session, pending_state: &mut PendingState) {
    eprintln!("greetd: shutting down");
    pending_state.cancel_pending();

    let _ = kill(greeter.pid(), Signal::SIGTERM);
    std::thread::sleep(Duration::from_millis(500));
    let _ = kill(greeter.pid(), Signal::SIGKILL);
    let _ = fs::remove_file(sock_path);
}

fn setup_signal_handlers() -> Result<(), Error> {
    let ignore = SigAction::new(SigHandler::SigIgn, SaFlags::empty(), SigSet::empty());
    unsafe { sigaction(Signal::SIGCHLD, &ignore)? };

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
            terminal_mode_from_device("/dev/tty0", config, Terminal::current_vt)
        }
        VtSelection::Next => terminal_mode_from_device("/dev/tty0", config, Terminal::next_vt),
        VtSelection::Specific(vt) => Ok(TerminalMode::Vt {
            path: format!("/dev/tty{vt}"),
            vt: *vt,
            switch: config.switch,
        }),
    }
}

fn terminal_mode_from_device(
    path: &str,
    config: &Config,
    get_vt: fn(&Terminal) -> Result<u32, Error>,
) -> Result<TerminalMode, Error> {
    let term = Terminal::open(path)?;
    let vt = get_vt(&term)?;
    Ok(TerminalMode::Vt {
        path: format!("/dev/tty{vt}"),
        vt,
        switch: config.switch && matches!(config.vt, VtSelection::Next),
    })
}

fn find_pam_service(name: &str) -> Result<String, Error> {
    for dir in &["/etc/pam.d", "/usr/lib/pam.d"] {
        if Path::new(&format!("{dir}/{name}")).exists() {
            return Ok(name.to_string());
        }
    }
    Err(format!("PAM service not found: {name}").into())
}

struct DirectSessionRequest<'a> {
    service: &'a str,
    class: SessionClass,
    user: &'a str,
    cmd: Vec<String>,
}

fn start_session_direct(
    runtime: &Runtime,
    request: DirectSessionRequest<'_>,
) -> Result<Session, Error> {
    let mut session = Session::spawn()?;
    session.initiate(
        request.service,
        request.class,
        request.user,
        false,
        &runtime.term_mode,
        runtime.source_profile,
        &runtime.sock_path,
    )?;

    while session.get_auth_prompt()?.is_some() {
        session.respond(None)?;
    }

    session.start(request.cmd, vec![])?;
    Ok(session)
}

fn start_greeter(runtime: &Runtime) -> Result<Session, Error> {
    let request = DirectSessionRequest {
        service: &runtime.greeter_pam_service,
        class: SessionClass::Greeter,
        user: &runtime.greeter_user,
        cmd: vec![runtime.greeter_command.clone()],
    };
    start_session_direct(runtime, request)
}

fn handle_client(mut stream: UnixStream, context: &mut ClientContext<'_>) -> Result<(), Error> {
    let mut session: Option<Session> = None;

    loop {
        let Some(request) = Request::read_from(&mut stream)? else {
            break;
        };

        let response = handle_request(request, &mut session, context)?;
        response.write_to(&mut stream)?;
    }

    cancel_session(&mut session);
    Ok(())
}

fn handle_request(
    request: Request,
    session: &mut Option<Session>,
    context: &mut ClientContext<'_>,
) -> Result<Response, Error> {
    match request {
        Request::CreateSession { username } => create_session_response(username, session, context),
        Request::PostAuthMessageResponse { response } => handle_auth_response(response, session),
        Request::StartSession { cmd, env } => start_session_response(cmd, env, session, context),
        Request::CancelSession => Ok(cancel_session_response(session)),
    }
}

fn create_session_response(
    username: String,
    session: &mut Option<Session>,
    context: &ClientContext<'_>,
) -> Result<Response, Error> {
    cancel_session(session);

    let mut new_session = Session::spawn()?;
    new_session.initiate(
        &context.runtime.pam_service,
        SessionClass::User,
        &username,
        true,
        &context.runtime.term_mode,
        context.runtime.source_profile,
        &context.runtime.sock_path,
    )?;

    let response = auth_prompt_response(&mut new_session)?;
    *session = Some(new_session);
    Ok(response)
}

fn handle_auth_response(
    response: Option<String>,
    session: &mut Option<Session>,
) -> Result<Response, Error> {
    let Some(session) = session.as_mut() else {
        return Ok(Response::error("no session active"));
    };

    session.respond(response)?;
    auth_prompt_response(session)
}

fn start_session_response(
    cmd: Vec<String>,
    env: Vec<String>,
    session: &mut Option<Session>,
    context: &mut ClientContext<'_>,
) -> Result<Response, Error> {
    let Some(mut session) = session.take() else {
        return Ok(Response::error("no session active"));
    };

    match session.start(cmd.clone(), env) {
        Ok(_) => {
            eprintln!("greetd: session started: {}", cmd.join(" "));
            context.pending_state.queue(session);
            Ok(Response::Success)
        }
        Err(error) => Ok(Response::error(error.to_string())),
    }
}

fn cancel_session_response(session: &mut Option<Session>) -> Response {
    cancel_session(session);
    Response::Success
}

fn cancel_session(session: &mut Option<Session>) {
    if let Some(mut session) = session.take() {
        let _ = session.cancel();
    }
}

fn auth_prompt_response(session: &mut Session) -> Result<Response, Error> {
    match session.get_auth_prompt()? {
        Some((prompt, echo)) => Ok(Response::AuthMessage {
            auth_message_type: if echo {
                AuthMessageType::Visible
            } else {
                AuthMessageType::Secret
            },
            auth_message: prompt,
        }),
        None => Ok(Response::Success),
    }
}
