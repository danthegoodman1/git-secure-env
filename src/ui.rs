//! Messages for people at a terminal. Command output goes to stdout; progress
//! and warnings go to stderr so they never mix with output a script reads.

use std::fmt::Display;
use std::io::{BufRead, IsTerminal, Write};

pub fn info(msg: impl Display) {
    eprintln!("gse: {msg}");
}

pub fn warn(msg: impl Display) {
    eprintln!("gse: warning: {msg}");
}

pub fn error(msg: impl Display) {
    eprintln!("gse: error: {msg}");
}

/// Asks a question on the terminal and returns the answer, or `default` when
/// stdin is not a terminal or the answer is empty.
pub fn prompt(question: &str, default: &str) -> String {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        return default.to_string();
    }
    eprint!("{question} [{default}]: ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if stdin.lock().read_line(&mut line).is_err() {
        return default.to_string();
    }
    let answer = line.trim();
    if answer.is_empty() {
        default.to_string()
    } else {
        answer.to_string()
    }
}
