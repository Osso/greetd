use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::{fork, setsid, setuid, setgid, initgroups, execve, ForkResult, Pid};
use std::ffi::CString;
use std::os::unix::net::UnixDatagram;
use serde::{Deserialize, Serialize};

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
    Start { cmd: Vec<String>, env: Vec<String> },
    Cancel,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum WorkerResponse {
    AuthPrompt { prompt: String, echo: bool },
    Ready,
    Started { pid: u32 },
    Error(String),
}

pub struct Session {
    sock: UnixDatagram,
    worker_pid: Pid,
}

impl Session {
    pub fn spawn() -> Result<Self, Error> {
        let (parent_sock, child_sock) = UnixDatagram::pair()?;

        match unsafe { fork() }? {
            ForkResult::Parent { child } => {
                drop(child_sock);
                Ok(Session { sock: parent_sock, worker_pid: child })
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
        let req = WorkerRequest::Initiate {
            service: service.into(),
            class,
            user: user.into(),
            authenticate,
            tty: tty.clone(),
            source_profile,
            greetd_sock: greetd_sock.into(),
        };
        send_msg(&self.sock, &req)?;
        Ok(())
    }

    pub fn get_auth_prompt(&mut self) -> Result<Option<(String, bool)>, Error> {
        match recv_msg(&self.sock)? {
            WorkerResponse::AuthPrompt { prompt, echo } => Ok(Some((prompt, echo))),
            WorkerResponse::Ready => Ok(None),
            WorkerResponse::Error(e) => Err(Error::Auth(e)),
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
            WorkerResponse::Error(e) => Err(e.into()),
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
    let data = serde_json::to_vec(msg).map_err(|e| Error::Other(e.to_string()))?;
    sock.send(&data)?;
    Ok(())
}

fn recv_msg<T: for<'de> Deserialize<'de>>(sock: &UnixDatagram) -> Result<T, Error> {
    let mut buf = [0u8; 8192];
    let len = sock.recv(&mut buf)?;
    serde_json::from_slice(&buf[..len]).map_err(|e| Error::Other(e.to_string()))
}

fn worker_main(sock: &UnixDatagram) -> Result<(), Error> {
    use pam::Client;

    let mut buf = [0u8; 8192];
    let len = sock.recv(&mut buf)?;
    let req: WorkerRequest = serde_json::from_slice(&buf[..len])
        .map_err(|e| Error::Other(e.to_string()))?;

    let (service, class, user, authenticate, tty, source_profile, greetd_sock) = match req {
        WorkerRequest::Initiate { service, class, user, authenticate, tty, source_profile, greetd_sock } => {
            (service, class, user, authenticate, tty, source_profile, greetd_sock)
        }
        WorkerRequest::Cancel => return Ok(()),
        _ => return Err("expected Initiate".into()),
    };

    // Create PAM client with password-based conversation
    let mut client = Client::with_password(&service)
        .map_err(|e| Error::Auth(format!("PAM init failed: {e}")))?;

    // For now, use simple password auth - real impl would need conversation
    if authenticate {
        // Send prompt to parent
        send_msg(sock, &WorkerResponse::AuthPrompt {
            prompt: "Password: ".into(),
            echo: false,
        })?;

        // Get response
        let len = sock.recv(&mut buf)?;
        let resp: WorkerRequest = serde_json::from_slice(&buf[..len])
            .map_err(|e| Error::Other(e.to_string()))?;

        let password = match resp {
            WorkerRequest::AuthResponse(Some(p)) => p,
            WorkerRequest::AuthResponse(None) => String::new(),
            WorkerRequest::Cancel => return Ok(()),
            _ => return Err("expected AuthResponse".into()),
        };

        client.conversation_mut().set_credentials(&user, &password);

        client.authenticate()
            .map_err(|e| Error::Auth(format!("authentication failed: {e}")))?;
    } else {
        // For unauthenticated sessions, just set credentials without password
        client.conversation_mut().set_credentials(&user, "");
    }

    send_msg(sock, &WorkerResponse::Ready)?;

    // Wait for start command
    let len = sock.recv(&mut buf)?;
    let req: WorkerRequest = serde_json::from_slice(&buf[..len])
        .map_err(|e| Error::Other(e.to_string()))?;

    let (cmd, env) = match req {
        WorkerRequest::Start { cmd, env } => (cmd, env),
        WorkerRequest::Cancel => return Ok(()),
        _ => return Err("expected Start".into()),
    };

    // Open PAM session
    client.open_session()
        .map_err(|e| Error::Other(format!("failed to open session: {e}")))?;

    // Get user info
    let user_info = nix::unistd::User::from_name(&user)?
        .ok_or_else(|| Error::Other(format!("user not found: {user}")))?;

    // Become session leader
    setsid()?;

    // Setup terminal if needed
    if let TerminalMode::Vt { path, vt, switch } = &tty {
        use crate::terminal::Terminal;
        let term = Terminal::open(path)?;
        term.set_text_mode()?;
        if *switch {
            term.activate(*vt)?;
        }
    }

    // Build environment
    let mut final_env: Vec<CString> = vec![
        CString::new(format!("HOME={}", user_info.dir.display()))?,
        CString::new(format!("USER={}", user_info.name))?,
        CString::new(format!("LOGNAME={}", user_info.name))?,
        CString::new(format!("SHELL={}", user_info.shell.display()))?,
        CString::new("TERM=linux")?,
        CString::new("XDG_SEAT=seat0")?,
    ];

    if let SessionClass::Greeter = class {
        final_env.push(CString::new(format!("GREETD_SOCK={greetd_sock}"))?);
    }

    for e in env {
        final_env.push(CString::new(e)?);
    }

    // Fork child to exec user session
    match unsafe { fork() }? {
        ForkResult::Parent { child } => {
            send_msg(sock, &WorkerResponse::Started { pid: child.as_raw() as u32 })?;

            // Wait for child
            loop {
                match waitpid(child, None) {
                    Ok(_) => break,
                    Err(nix::errno::Errno::EINTR) => continue,
                    Err(e) => {
                        eprintln!("waitpid failed: {e}");
                        break;
                    }
                }
            }

            // Close PAM session (handled by Drop)
            drop(client);
            Ok(())
        }
        ForkResult::Child => {
            // Drop privileges
            let cuser = CString::new(user_info.name.as_str())?;
            initgroups(&cuser, user_info.gid)?;
            setgid(user_info.gid)?;
            setuid(user_info.uid)?;

            // Change to home directory
            let _ = std::env::set_current_dir(&user_info.dir);

            // Build command
            let shell = CString::new("/bin/sh")?;
            let command = if source_profile {
                format!(
                    "[ -f /etc/profile ] && . /etc/profile; [ -f $HOME/.profile ] && . $HOME/.profile; exec {}",
                    cmd.join(" ")
                )
            } else {
                format!("exec {}", cmd.join(" "))
            };

            let args = [
                shell.clone(),
                CString::new("-c")?,
                CString::new(command)?,
            ];

            let _ = execve(&shell, &args, &final_env);
            std::process::exit(1)
        }
    }
}

/// Check if any child processes have exited
pub fn reap_children() -> Option<(Pid, i32)> {
    match waitpid(None, Some(WaitPidFlag::WNOHANG)) {
        Ok(WaitStatus::Exited(pid, code)) => Some((pid, code)),
        Ok(WaitStatus::Signaled(pid, _, _)) => Some((pid, 1)),
        _ => None,
    }
}
