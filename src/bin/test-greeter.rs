//! Minimal test greeter for Docker testing.
//!
//! Reads GREETD_SOCK from environment, authenticates a user, and starts a session.
//!
//! Usage: test-greeter <username> <password> <command>
//! Example: test-greeter testuser testpass /bin/bash

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

fn main() {
    let (username, password, command) = parse_args();
    let mut stream = connect_greetd();

    let response = create_session(&mut stream, &username);
    authenticate_if_needed(&mut stream, &response, &password);
    start_session(&mut stream, &command);
}

fn parse_args() -> (String, String, String) {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("Usage: test-greeter <username> <password> <command>");
        std::process::exit(1);
    }

    (args[1].clone(), args[2].clone(), args[3].clone())
}

fn connect_greetd() -> UnixStream {
    let sock_path = std::env::var("GREETD_SOCK").expect("GREETD_SOCK not set");
    eprintln!("Connecting to {sock_path}");
    UnixStream::connect(&sock_path).expect("Failed to connect to socket")
}

fn create_session(stream: &mut UnixStream, username: &str) -> String {
    let request = format!(r#"{{"type":"create_session","username":"{username}"}}"#);
    send(stream, &request);
    let response = recv(stream);
    eprintln!("create_session -> {response}");
    response
}

fn authenticate_if_needed(stream: &mut UnixStream, response: &str, password: &str) {
    if !response.contains("auth_message") {
        return;
    }

    let request = format!(r#"{{"type":"post_auth_message_response","response":"{password}"}}"#);
    send(stream, &request);
    let response = recv(stream);
    eprintln!("post_auth_message_response -> {response}");

    if response.contains("error") {
        eprintln!("Authentication failed");
        std::process::exit(1);
    }
}

fn start_session(stream: &mut UnixStream, command: &str) {
    let request = format!(r#"{{"type":"start_session","cmd":["{command}"]}}"#);
    send(stream, &request);
    let response = recv(stream);
    eprintln!("start_session -> {response}");

    if response.contains("success") {
        eprintln!("Session started, greeter exiting");
        return;
    }

    eprintln!("Failed to start session");
    std::process::exit(1);
}

fn send(stream: &mut UnixStream, json: &str) {
    let len = (json.len() as u32).to_ne_bytes();
    stream.write_all(&len).expect("Failed to write length");
    stream
        .write_all(json.as_bytes())
        .expect("Failed to write JSON");
}

fn recv(stream: &mut UnixStream) -> String {
    let mut len_buf = [0u8; 4];
    stream
        .read_exact(&mut len_buf)
        .expect("Failed to read length");
    let len = u32::from_ne_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).expect("Failed to read JSON");
    String::from_utf8(buf).expect("Invalid UTF-8")
}
