//! The trusted parent process owns stdin/stdout. This entrypoint is never used by the TCP
//! listener: its connections still require a token via `Connection::new`.
use std::io::{BufRead, Write};

use nodecraft_engine::Session;
use nodecraft_types::{Error, ErrorKind};
use serde_json::{Value, json};

use super::{Connection, Step, answer, error_reply, error_value};
use crate::mcp::{Line, MAX_MESSAGE_BYTES, read_line};

/// Serve headless control in order until EOF. The caller explicitly trusts the parent process;
/// no token is generated or required. `hello` remains available for version negotiation.
/// Empty lines are ignored; malformed, non-UTF-8 and oversized lines return one error and recover
/// at the next line. A final unterminated line is processed. I/O failure ends the transport.
/// This reuses TCP's parser and command admission; it cannot grant UI/keychain capabilities.
pub fn serve(session: &mut Session, input: &mut dyn BufRead, output: &mut dyn Write) -> std::io::Result<()> {
    // Private construction is confined to this trusted transport, never exported as a TCP mode.
    let mut connection = Connection { token: None, authed: true };
    let mut line = Vec::new();
    loop {
        line.clear();
        let step = match read_line(input, &mut line)? {
            Line::Eof => return Ok(()),
            Line::TooLong => Step::Reply(error_reply(Value::Null, "invalid_input", &format!("message is larger than {MAX_MESSAGE_BYTES} bytes"))),
            Line::Read => match std::str::from_utf8(&line) {
                Ok(text) if text.trim().is_empty() => continue,
                Ok(text) => connection.step(text.trim()),
                Err(_) => Step::Reply(error_reply(Value::Null, "invalid_input", "the message isn't valid UTF-8")),
            },
        };
        let (reply, close) = match step {
            Step::Reply(reply) => (reply, false),
            Step::Close(reply) => (reply, true),
            Step::Ask(id, request) => {
                let result =
                    answer(session, &request).unwrap_or_else(|| Err(Error::new(ErrorKind::Unsupported, "screenshots require a running desktop window")));
                (
                    match result {
                        Ok(result) => json!({"id": id, "result": result}),
                        Err(error) => error_value(id, &error),
                    },
                    false,
                )
            }
        };
        writeln!(output, "{reply}")?;
        output.flush()?;
        if close {
            return Ok(());
        }
    }
}
