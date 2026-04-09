//! Minimal test greeter for Docker testing.
//!
//! Reads GREETD_SOCK from environment, authenticates a user, and starts a session.
//!
//! Usage: test-greeter <username> <password> <command>
//! Example: test-greeter testuser testpass /bin/bash

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("Usage: test-greeter <username> <password> <command>");
        std::process::exit(1);
    }

    let username = &args[1];
    let password = &args[2];
    let command = &args[3];

    let sock_path = std::env::var("GREETD_SOCK").expect("GREETD_SOCK not set");
    eprintln!("Connecting to {sock_path}");

    let mut stream = UnixStream::connect(&sock_path).expect("Failed to connect to socket");

    // Step 1: Create session
    let req = format!(r#"{{"type":"create_session","username":"{username}"}}"#);
    send(&mut stream, &req);
    let resp = recv(&mut stream);
    eprintln!("create_session -> {resp}");

    // Check if we got an auth prompt or success
    if resp.contains("auth_message") {
        // Step 2: Respond to auth prompt
        let req = format!(r#"{{"type":"post_auth_message_response","response":"{password}"}}"#);
        send(&mut stream, &req);
        let resp = recv(&mut stream);
        eprintln!("post_auth_message_response -> {resp}");

        if resp.contains("error") {
            eprintln!("Authentication failed");
            std::process::exit(1);
        }
    }

    // Step 3: Start session
    let req = format!(r#"{{"type":"start_session","cmd":["{command}"]}}"#);
    send(&mut stream, &req);
    let resp = recv(&mut stream);
    eprintln!("start_session -> {resp}");

    if resp.contains("success") {
        eprintln!("Session started, greeter exiting");
    } else {
        eprintln!("Failed to start session");
        std::process::exit(1);
    }
}

fn send(stream: &mut UnixStream, json: &str) {
    let len = (json.len() as u32).to_ne_bytes();
    stream.write_all(&len).expect("Failed to write length");
    stream.write_all(json.as_bytes()).expect("Failed to write JSON");
}

fn recv(stream: &mut UnixStream) -> String {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).expect("Failed to read length");
    let len = u32::from_ne_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).expect("Failed to read JSON");
    String::from_utf8(buf).expect("Invalid UTF-8")
}
