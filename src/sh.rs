use anyhow::{bail, Result};
use serde_json::Value;
use std::fmt;
use std::io::Write;
use std::process::{Command, Stdio};

/// A refusal: printed `REFUSED <code>: <why>`, exit 3.
#[derive(Debug)]
pub struct Refused {
    pub code: &'static str,
    pub why: String,
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "REFUSED {}: {}", self.code, self.why)
    }
}

impl std::error::Error for Refused {}

/// Refuse with `code`.
pub fn refused<T>(code: &'static str, why: impl Into<String>) -> Result<T> {
    Err(Refused { code, why: why.into() }.into())
}

/// A finished process: exit code and output.
pub struct Out {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Where a command runs: this host, or another over ssh.
#[derive(Clone, Copy, Default)]
pub struct On<'a>(pub Option<&'a str>);

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

impl On<'_> {
    /// Run `prog` with `args`, `input` on its stdin.
    pub fn run_in(self, prog: &str, args: &[&str], input: &str) -> Result<Out> {
        let mut c = match self.0 {
            None => {
                let mut c = Command::new(prog);
                c.args(args);
                c
            }
            Some(host) => {
                let line: Vec<String> = std::iter::once(prog).chain(args.iter().copied()).map(quote).collect();
                let mut c = Command::new("ssh");
                c.args(["-oBatchMode=yes", "-oConnectTimeout=10", host, "--", &line.join(" ")]);
                c
            }
        };
        let mut child = c
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| anyhow::anyhow!("{prog}: {e}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(input.as_bytes())?;
        }
        let o = child.wait_with_output()?;
        Ok(Out {
            code: o.status.code().unwrap_or(1),
            stdout: String::from_utf8_lossy(&o.stdout).into(),
            stderr: String::from_utf8_lossy(&o.stderr).into(),
        })
    }
    /// `prog`'s stdout when it exits 0, else its stderr as the error.
    pub fn ok(self, prog: &str, args: &[&str]) -> Result<String> {
        let o = self.run_in(prog, args, "")?;
        if o.code != 0 {
            bail!("{prog} {}: {}", args.join(" "), o.stderr.trim());
        }
        Ok(o.stdout)
    }
}

/// `prog` on this host: its stdout when it exits 0.
pub fn ok(prog: &str, args: &[&str]) -> Result<String> {
    On::default().ok(prog, args)
}

/// `prog` on this host, `input` on its stdin.
pub fn run_in(prog: &str, args: &[&str], input: &str) -> Result<Out> {
    On::default().run_in(prog, args, input)
}

/// `prog`'s stdout as JSON.
pub fn json(prog: &str, args: &[&str]) -> Result<Value> {
    Ok(serde_json::from_str(&ok(prog, args)?)?)
}

/// `cmd` as one shell line: each word quoted where it needs it.
pub fn line(cmd: &[String]) -> String {
    let plain = |w: &str| {
        !w.is_empty()
            && w.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_./:=@,+%".contains(&b))
    };
    let words: Vec<String> = (cmd.iter())
        .map(|w| if plain(w) { w.clone() } else { quote(w) })
        .collect();
    words.join(" ")
}
