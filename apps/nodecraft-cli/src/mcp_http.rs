//! `nodecraft-cli mcp --listen [address]`: the MCP server over Streamable HTTP (#116).
//!
//! The transport lives in `nodecraft_automation::mcp::http`; this module picks the address and
//! the token and says where to connect. The token is `NODECRAFT_MCP_TOKEN` when that is set, or
//! a fresh one shown once on stderr; it is never written anywhere.

use std::io::Write;
use std::net::SocketAddr;
use std::process::ExitCode;

use nodecraft_automation::mcp::McpServer;
use nodecraft_automation::mcp::http::{self, BearerToken, Config};

/// Where `--listen` without an address listens. The desktop app's own server uses 41116.
pub const DEFAULT_LISTEN: &str = "127.0.0.1:41117";
/// The variable that chooses the token instead of a generated one.
pub const TOKEN_VAR: &str = "NODECRAFT_MCP_TOKEN";

/// The address `--listen` was given (or the default), refused unless it is on this computer.
pub fn listen_addr(arg: Option<&str>) -> Result<SocketAddr, String> {
    let text = arg.unwrap_or(DEFAULT_LISTEN);
    let addr: SocketAddr = text.parse().map_err(|_| format!("`{}` isn't an address such as {DEFAULT_LISTEN}", text.chars().take(80).collect::<String>()))?;
    if !addr.ip().is_loopback() {
        return Err("the MCP server listens only on this computer: use 127.0.0.1 or [::1]".into());
    }
    Ok(addr)
}

/// The token: from `value` (the variable) when set, else generated. `true` when generated, so
/// it is shown.
pub fn token(value: Option<std::ffi::OsString>) -> Result<(BearerToken, bool), String> {
    match value {
        Some(value) => {
            let value = value.into_string().map_err(|_| format!("{TOKEN_VAR} isn't valid text"))?;
            BearerToken::new(value.trim()).map(|t| (t, false)).map_err(|e| format!("{TOKEN_VAR}: {}", e.message))
        }
        None => BearerToken::generate().map(|t| (t, true)).map_err(|e| e.message),
    }
}

/// Serves `server` over HTTP on `addr` until the process is stopped.
pub fn serve(server: &mut McpServer, addr: SocketAddr) -> ExitCode {
    let (token, shown) = match token(std::env::var_os(TOKEN_VAR)) {
        Ok(t) => t,
        Err(e) => return crate::fail(&e),
    };
    let listening = match http::bind(Config::new(addr, token.clone())) {
        Ok(l) => l,
        Err(e) => return crate::fail(&e.message),
    };
    let url = listening.url();
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "NodeCraft MCP server on {url}");
    if shown {
        let _ = writeln!(err, "token (shown once; set {TOKEN_VAR} to choose your own): {}", token.expose());
        let _ = writeln!(err, "  claude mcp add --transport http nodecraft {url} --header \"Authorization: Bearer {}\"", token.expose());
    } else {
        let _ = writeln!(err, "token: from {TOKEN_VAR} ({})", token.masked());
    }
    drop(err);
    listening.serve(&mut |message| server.handle_message(message));
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listens_only_on_loopback_and_takes_a_chosen_token() {
        assert_eq!(listen_addr(None).unwrap(), DEFAULT_LISTEN.parse().unwrap());
        assert_eq!(listen_addr(Some("[::1]:9")).unwrap(), "[::1]:9".parse().unwrap());
        for bad in ["0.0.0.0:41117", "192.168.1.2:80", "localhost:80", "nonsense"] {
            assert!(listen_addr(Some(bad)).is_err(), "{bad}");
        }
        let chosen = "c".repeat(40);
        let (chosen_token, shown) = token(Some(chosen.clone().into())).unwrap();
        assert!(!shown && chosen_token.matches(&chosen));
        assert!(token(Some("short".into())).is_err());
        let (_, shown) = token(None).unwrap();
        assert!(shown);
    }

    #[test]
    fn mcp_takes_bridge_and_listen_in_either_order() {
        let args = |a: &[&str]| crate::mcp_args(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(args(&[]).unwrap(), crate::McpArgs::default());
        let both = args(&["--listen", "--bridge", "/t.json"]).unwrap();
        assert_eq!(both.bridge, Some(Some("/t.json".into())));
        assert_eq!(both.listen, Some(DEFAULT_LISTEN.parse().unwrap()));
        let both = args(&["--bridge", "--listen", "127.0.0.1:9"]).unwrap();
        assert_eq!(both.bridge, Some(None));
        assert_eq!(both.listen, Some("127.0.0.1:9".parse().unwrap()));
        for bad in [&["--listen", "0.0.0.0:1"][..], &["--bridge", "--bridge"], &["--port"], &["extra"]] {
            assert!(args(bad).is_err(), "{bad:?}");
        }
    }
}
