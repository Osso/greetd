#[cfg(not(coverage))]
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
#[cfg(not(coverage))]
use nix::unistd::{ForkResult, Pid, execve, fork, initgroups, setgid, setsid, setuid};
use serde::{Deserialize, Serialize};
use std::ffi::CString;
use std::os::unix::net::UnixDatagram;

use crate::error::Error;
use crate::terminal::TerminalMode;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SessionClass {
    Greeter,
    User,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum WorkerRequest {
    Initiate {
        service: String,
        class: SessionClass,
        user: String,
        authenticate: bool,
        tty: TerminalMode,
        source_profile: bool,
        greetd_sock: String,
    },
    AuthResponse(Option<String>),
    Start {
        cmd: Vec<String>,
        env: Vec<String>,
    },
    Cancel,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum WorkerResponse {
    AuthPrompt { prompt: String, echo: bool },
    Ready,
    Started { pid: u32 },
    Error(String),
}

#[derive(Debug)]
struct InitiateRequest {
    service: String,
    class: SessionClass,
    user: String,
    authenticate: bool,
    tty: TerminalMode,
    source_profile: bool,
    greetd_sock: String,
}

#[derive(Debug)]
struct StartRequest {
    cmd: Vec<String>,
    env: Vec<String>,
}

#[cfg(not(coverage))]
pub struct Session {
    sock: UnixDatagram,
    worker_pid: Pid,
}

#[cfg(not(coverage))]
impl Session {
    pub fn spawn() -> Result<Self, Error> {
        let (parent_sock, child_sock) = UnixDatagram::pair()?;

        match unsafe { fork() }? {
            ForkResult::Parent { child } => {
                drop(child_sock);
                Ok(Session {
                    sock: parent_sock,
                    worker_pid: child,
                })
            }
            ForkResult::Child => {
                drop(parent_sock);
                let result = worker_main(&child_sock);
                std::process::exit(if result.is_ok() { 0 } else { 1 });
            }
        }
    }

    pub fn initiate(
        &mut self,
        service: &str,
        class: SessionClass,
        user: &str,
        authenticate: bool,
        tty: &TerminalMode,
        source_profile: bool,
        greetd_sock: &str,
    ) -> Result<(), Error> {
        let request = WorkerRequest::Initiate {
            service: service.into(),
            class,
            user: user.into(),
            authenticate,
            tty: tty.clone(),
            source_profile,
            greetd_sock: greetd_sock.into(),
        };
        send_msg(&self.sock, &request)
    }

    pub fn get_auth_prompt(&mut self) -> Result<Option<(String, bool)>, Error> {
        match recv_msg(&self.sock)? {
            WorkerResponse::AuthPrompt { prompt, echo } => Ok(Some((prompt, echo))),
            WorkerResponse::Ready => Ok(None),
            WorkerResponse::Error(error) => Err(Error::Auth(error)),
            WorkerResponse::Started { .. } => Err("unexpected Started".into()),
        }
    }

    pub fn respond(&mut self, response: Option<String>) -> Result<(), Error> {
        send_msg(&self.sock, &WorkerRequest::AuthResponse(response))
    }

    pub fn start(&mut self, cmd: Vec<String>, env: Vec<String>) -> Result<u32, Error> {
        send_msg(&self.sock, &WorkerRequest::Start { cmd, env })?;
        match recv_msg(&self.sock)? {
            WorkerResponse::Started { pid } => Ok(pid),
            WorkerResponse::Error(error) => Err(error.into()),
            _ => Err("unexpected response".into()),
        }
    }

    pub fn cancel(&mut self) -> Result<(), Error> {
        send_msg(&self.sock, &WorkerRequest::Cancel)
    }

    pub fn pid(&self) -> Pid {
        self.worker_pid
    }
}

fn send_msg<T: Serialize>(sock: &UnixDatagram, msg: &T) -> Result<(), Error> {
    let data = serde_json::to_vec(msg).map_err(|error| Error::Other(error.to_string()))?;
    loop {
        match sock.send(&data) {
            Ok(_) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
}

fn recv_msg<T: for<'de> Deserialize<'de>>(sock: &UnixDatagram) -> Result<T, Error> {
    let mut buf = [0u8; 8192];
    let len = loop {
        match sock.recv(&mut buf) {
            Ok(len) => break len,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    };
    serde_json::from_slice(&buf[..len]).map_err(|error| Error::Other(error.to_string()))
}

#[cfg(not(coverage))]
fn worker_main(sock: &UnixDatagram) -> Result<(), Error> {
    use pam::Client;

    let Some(initiate) = receive_initiate(sock)? else {
        return Ok(());
    };
    let mut client = Client::with_password(&initiate.service)
        .map_err(|error| Error::Auth(format!("PAM init failed: {error}")))?;

    authenticate_worker(sock, &mut client, &initiate)?;
    send_msg(sock, &WorkerResponse::Ready)?;

    let Some(start) = receive_start(sock)? else {
        return Ok(());
    };

    client
        .open_session()
        .map_err(|error| Error::Other(format!("failed to open session: {error}")))?;

    let user_info = load_user(&initiate.user)?;
    setsid()?;
    prepare_terminal(&initiate.tty)?;

    let final_env = build_environment(
        &user_info,
        start.env,
        &initiate.class,
        &initiate.greetd_sock,
    )?;
    spawn_session_process(
        sock,
        client,
        user_info,
        start.cmd,
        final_env,
        initiate.source_profile,
    )
}

fn receive_initiate(sock: &UnixDatagram) -> Result<Option<InitiateRequest>, Error> {
    match recv_msg(sock)? {
        WorkerRequest::Initiate {
            service,
            class,
            user,
            authenticate,
            tty,
            source_profile,
            greetd_sock,
        } => Ok(Some(InitiateRequest {
            service,
            class,
            user,
            authenticate,
            tty,
            source_profile,
            greetd_sock,
        })),
        WorkerRequest::Cancel => Ok(None),
        _ => Err("expected Initiate".into()),
    }
}

#[cfg(not(coverage))]
fn authenticate_worker(
    sock: &UnixDatagram,
    client: &mut pam::Client<pam::PasswordConv>,
    initiate: &InitiateRequest,
) -> Result<(), Error> {
    if !initiate.authenticate {
        client
            .conversation_mut()
            .set_credentials(&initiate.user, "");
        client
            .authenticate()
            .map_err(|error| Error::Auth(format!("authentication failed: {error}")))?;
        return Ok(());
    }

    let password = authenticate_with_password_prompt(sock, client, &initiate.user)?;

    #[cfg(feature = "keyring")]
    unlock_keyring(&initiate.user, &password);

    Ok(())
}

#[cfg(not(coverage))]
fn authenticate_with_password_prompt(
    sock: &UnixDatagram,
    client: &mut pam::Client<pam::PasswordConv>,
    user: &str,
) -> Result<String, Error> {
    send_msg(
        sock,
        &WorkerResponse::AuthPrompt {
            prompt: "Password: ".into(),
            echo: false,
        },
    )?;

    let password = receive_password(sock)?;
    client.conversation_mut().set_credentials(user, &password);
    client
        .authenticate()
        .map_err(|error| Error::Auth(format!("authentication failed: {error}")))?;
    Ok(password)
}

fn receive_password(sock: &UnixDatagram) -> Result<String, Error> {
    match recv_msg(sock)? {
        WorkerRequest::AuthResponse(Some(password)) => Ok(password),
        WorkerRequest::AuthResponse(None) => Ok(String::new()),
        WorkerRequest::Cancel => Ok(String::new()),
        _ => Err("expected AuthResponse".into()),
    }
}

fn receive_start(sock: &UnixDatagram) -> Result<Option<StartRequest>, Error> {
    match recv_msg(sock)? {
        WorkerRequest::Start { cmd, env } => Ok(Some(StartRequest { cmd, env })),
        WorkerRequest::Cancel => Ok(None),
        _ => Err("expected Start".into()),
    }
}

#[cfg(not(coverage))]
fn load_user(user: &str) -> Result<nix::unistd::User, Error> {
    nix::unistd::User::from_name(user)?
        .ok_or_else(|| Error::Other(format!("user not found: {user}")))
}

#[cfg(not(coverage))]
fn prepare_terminal(tty: &TerminalMode) -> Result<(), Error> {
    let TerminalMode::Vt { path, vt, switch } = tty else {
        return Ok(());
    };

    use crate::terminal::Terminal;

    let term = Terminal::open(path)?;
    term.set_text_mode()?;
    if *switch {
        term.activate(*vt)?;
    }

    Ok(())
}

fn build_environment(
    user_info: &nix::unistd::User,
    env: Vec<String>,
    class: &SessionClass,
    greetd_sock: &str,
) -> Result<Vec<CString>, Error> {
    let mut final_env = vec![
        CString::new(format!("HOME={}", user_info.dir.display()))?,
        CString::new(format!("USER={}", user_info.name))?,
        CString::new(format!("LOGNAME={}", user_info.name))?,
        CString::new(format!("SHELL={}", user_info.shell.display()))?,
        CString::new("TERM=linux")?,
        CString::new("XDG_SEAT=seat0")?,
    ];

    if matches!(class, SessionClass::Greeter) {
        final_env.push(CString::new(format!("GREETD_SOCK={greetd_sock}"))?);
    }

    for value in env {
        final_env.push(CString::new(value)?);
    }

    Ok(final_env)
}

#[cfg(not(coverage))]
fn spawn_session_process(
    sock: &UnixDatagram,
    client: pam::Client<pam::PasswordConv>,
    user_info: nix::unistd::User,
    cmd: Vec<String>,
    final_env: Vec<CString>,
    source_profile: bool,
) -> Result<(), Error> {
    match unsafe { fork() }? {
        ForkResult::Parent { child } => {
            send_msg(
                sock,
                &WorkerResponse::Started {
                    pid: child.as_raw() as u32,
                },
            )?;
            wait_for_session_child(child);
            drop(client);
            Ok(())
        }
        ForkResult::Child => {
            if let Err(error) = exec_session_command(user_info, cmd, final_env, source_profile) {
                eprintln!("greetd: exec failed: {error}");
            }
            std::process::exit(1);
        }
    }
}

#[cfg(not(coverage))]
fn wait_for_session_child(child: Pid) {
    loop {
        match waitpid(child, None) {
            Ok(_) => break,
            Err(nix::errno::Errno::EINTR) => continue,
            Err(error) => {
                eprintln!("waitpid failed: {error}");
                break;
            }
        }
    }
}

#[cfg(not(coverage))]
fn exec_session_command(
    user_info: nix::unistd::User,
    cmd: Vec<String>,
    final_env: Vec<CString>,
    source_profile: bool,
) -> Result<(), Error> {
    let cuser = CString::new(user_info.name.as_str())?;
    initgroups(&cuser, user_info.gid)?;
    setgid(user_info.gid)?;
    setuid(user_info.uid)?;

    let _ = std::env::set_current_dir(&user_info.dir);

    let shell = CString::new("/bin/sh")?;
    let command = shell_command(&cmd, source_profile);
    let args = [shell.clone(), CString::new("-c")?, CString::new(command)?];

    let _ = execve(&shell, &args, &final_env);
    Ok(())
}

fn shell_command(cmd: &[String], source_profile: bool) -> String {
    if source_profile {
        return format!(
            "[ -f /etc/profile ] && . /etc/profile; [ -f $HOME/.profile ] && . $HOME/.profile; exec {}",
            cmd.join(" ")
        );
    }

    format!("exec {}", cmd.join(" "))
}

/// Check if any child processes have exited
#[cfg(not(coverage))]
pub fn reap_children() -> Option<(Pid, i32)> {
    match waitpid(None, Some(WaitPidFlag::WNOHANG)) {
        Ok(WaitStatus::Exited(pid, code)) => Some((pid, code)),
        Ok(WaitStatus::Signaled(pid, _, _)) => Some((pid, 1)),
        _ => None,
    }
}

/// Unlock the keyring daemon with the user's login password
#[cfg(all(feature = "keyring", not(coverage)))]
fn unlock_keyring(user: &str, password: &str) {
    use keyring_protocol::{UNLOCK_SOCKET_PATH, UnlockRequest, UnlockResponse};
    use peercred_ipc::Client;

    let request = UnlockRequest {
        user: user.to_string(),
        password: password.to_string(),
    };

    match Client::call::<_, _, UnlockResponse>(UNLOCK_SOCKET_PATH, &request) {
        Ok(UnlockResponse::Success) => {
            eprintln!("greetd: keyring unlocked");
        }
        Ok(UnlockResponse::AlreadyUnlocked) => {}
        Ok(UnlockResponse::WrongPassword) => {
            eprintln!("greetd: keyring password mismatch (login password differs from keyring)");
        }
        Ok(UnlockResponse::Error { message }) => {
            eprintln!("greetd: keyring error: {}", message);
        }
        Err(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::unistd::{Gid, Uid, User};
    use std::path::PathBuf;

    fn socket_pair() -> (UnixDatagram, UnixDatagram) {
        UnixDatagram::pair().unwrap()
    }

    fn send_request(sock: &UnixDatagram, request: WorkerRequest) {
        send_msg(sock, &request).unwrap();
    }

    fn test_user() -> User {
        User {
            name: "alessio".into(),
            passwd: CString::new("x").unwrap(),
            uid: Uid::from_raw(1000),
            gid: Gid::from_raw(1000),
            gecos: CString::new("Alessio").unwrap(),
            dir: PathBuf::from("/home/alessio"),
            shell: PathBuf::from("/bin/zsh"),
        }
    }

    fn strings_from_env(env: Vec<CString>) -> Vec<String> {
        env.into_iter()
            .map(|value| value.into_string().unwrap())
            .collect()
    }

    #[test]
    fn receive_initiate_maps_request_fields() {
        let (client, server) = socket_pair();
        send_request(
            &client,
            WorkerRequest::Initiate {
                service: "greetd".into(),
                class: SessionClass::Greeter,
                user: "greeter".into(),
                authenticate: false,
                tty: TerminalMode::Stdin,
                source_profile: true,
                greetd_sock: "/run/greetd.sock".into(),
            },
        );

        let request = receive_initiate(&server).unwrap().unwrap();

        assert_eq!(request.service, "greetd");
        assert!(matches!(request.class, SessionClass::Greeter));
        assert_eq!(request.user, "greeter");
        assert!(!request.authenticate);
        assert!(matches!(request.tty, TerminalMode::Stdin));
        assert!(request.source_profile);
        assert_eq!(request.greetd_sock, "/run/greetd.sock");
    }

    #[test]
    fn worker_response_roundtrips_over_datagram() {
        let (client, server) = socket_pair();
        send_msg(
            &client,
            &WorkerResponse::AuthPrompt {
                prompt: "Password: ".into(),
                echo: false,
            },
        )
        .unwrap();

        let response: WorkerResponse = recv_msg(&server).unwrap();

        match response {
            WorkerResponse::AuthPrompt { prompt, echo } => {
                assert_eq!(prompt, "Password: ");
                assert!(!echo);
            }
            _ => panic!("expected auth prompt"),
        }
    }

    #[test]
    fn recv_msg_rejects_invalid_json() {
        let (client, server) = socket_pair();
        client.send(b"{not-json").unwrap();

        let error = recv_msg::<WorkerRequest>(&server).unwrap_err();

        assert!(matches!(error, Error::Other(message) if !message.is_empty()));
    }

    #[test]
    fn receive_initiate_cancel_returns_none() {
        let (client, server) = socket_pair();
        send_request(&client, WorkerRequest::Cancel);

        assert!(receive_initiate(&server).unwrap().is_none());
    }

    #[test]
    fn receive_initiate_rejects_wrong_message() {
        let (client, server) = socket_pair();
        send_request(
            &client,
            WorkerRequest::AuthResponse(Some("password".into())),
        );

        let error = receive_initiate(&server).unwrap_err().to_string();

        assert_eq!(error, "expected Initiate");
    }

    #[test]
    fn receive_password_accepts_password_none_and_cancel() {
        let (client, server) = socket_pair();
        send_request(&client, WorkerRequest::AuthResponse(Some("secret".into())));
        assert_eq!(receive_password(&server).unwrap(), "secret");

        send_request(&client, WorkerRequest::AuthResponse(None));
        assert_eq!(receive_password(&server).unwrap(), "");

        send_request(&client, WorkerRequest::Cancel);
        assert_eq!(receive_password(&server).unwrap(), "");
    }

    #[test]
    fn receive_password_rejects_wrong_message() {
        let (client, server) = socket_pair();
        send_request(
            &client,
            WorkerRequest::Start {
                cmd: vec!["sway".into()],
                env: vec![],
            },
        );

        let error = receive_password(&server).unwrap_err().to_string();

        assert_eq!(error, "expected AuthResponse");
    }

    #[test]
    fn receive_start_accepts_start_and_cancel() {
        let (client, server) = socket_pair();
        send_request(
            &client,
            WorkerRequest::Start {
                cmd: vec!["sway".into()],
                env: vec!["WLR_RENDERER=vulkan".into()],
            },
        );

        let request = receive_start(&server).unwrap().unwrap();
        assert_eq!(request.cmd, vec!["sway"]);
        assert_eq!(request.env, vec!["WLR_RENDERER=vulkan"]);

        send_request(&client, WorkerRequest::Cancel);
        assert!(receive_start(&server).unwrap().is_none());
    }

    #[test]
    fn receive_start_rejects_wrong_message() {
        let (client, server) = socket_pair();
        send_request(
            &client,
            WorkerRequest::AuthResponse(Some("password".into())),
        );

        let error = receive_start(&server).unwrap_err().to_string();

        assert_eq!(error, "expected Start");
    }

    #[test]
    fn build_environment_sets_base_user_values() {
        let env = build_environment(
            &test_user(),
            vec!["DISPLAY=:1".into()],
            &SessionClass::User,
            "/run/greetd.sock",
        )
        .unwrap();

        let env = strings_from_env(env);

        assert!(env.contains(&"HOME=/home/alessio".into()));
        assert!(env.contains(&"USER=alessio".into()));
        assert!(env.contains(&"LOGNAME=alessio".into()));
        assert!(env.contains(&"SHELL=/bin/zsh".into()));
        assert!(env.contains(&"TERM=linux".into()));
        assert!(env.contains(&"XDG_SEAT=seat0".into()));
        assert!(env.contains(&"DISPLAY=:1".into()));
        assert!(!env.iter().any(|value| value.starts_with("GREETD_SOCK=")));
    }

    #[test]
    fn build_environment_adds_greeter_socket() {
        let env = build_environment(
            &test_user(),
            vec![],
            &SessionClass::Greeter,
            "/run/greetd.sock",
        )
        .unwrap();

        let env = strings_from_env(env);

        assert!(env.contains(&"GREETD_SOCK=/run/greetd.sock".into()));
    }

    #[test]
    fn build_environment_rejects_nul_bytes() {
        let error = build_environment(
            &test_user(),
            vec!["BAD=value\0tail".into()],
            &SessionClass::User,
            "/run/greetd.sock",
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("nul byte"));
    }

    #[test]
    fn shell_command_without_profile_execs_command() {
        let cmd = vec!["sway".into(), "--unsupported-gpu".into()];

        assert_eq!(shell_command(&cmd, false), "exec sway --unsupported-gpu");
    }

    #[test]
    fn shell_command_with_profile_sources_profiles_first() {
        let cmd = vec!["sway".into()];
        let command = shell_command(&cmd, true);

        assert!(command.contains("[ -f /etc/profile ] && . /etc/profile"));
        assert!(command.contains("[ -f $HOME/.profile ] && . $HOME/.profile"));
        assert!(command.ends_with("exec sway"));
    }
}
