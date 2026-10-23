//! Running solutions and tests: run commands, timeouts, output capture.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::config::Config;
use crate::judge::harness;
use crate::judge::proc::{self, Cancel};
use crate::model::store::{Problem, Store};

/// Cap on captured output per stream, so a runaway print loop can't eat memory.
const MAX_OUTPUT: u64 = 256 * 1024;

/// Generates the harness if needed (blocking), builds the test file and runs it.
pub fn run_tests(
    store: &Store,
    cfg: &Config,
    p: &Problem,
    regenerate: bool,
    timeout: Duration,
    cancel: &Cancel,
) -> Result<(String, Option<(u32, u32)>)> {
    harness::ensure_has_solution(store, cfg, p)?;
    let source = if harness::needs_claude(store, cfg, p, regenerate) {
        harness::generate(store, cfg, p)?;
        harness::Source::Claude
    } else {
        harness::Source::Auto
    };
    run_existing_tests(store, cfg, p, source, timeout, cancel)
}

/// Runs tests with an existing harness (built-in or generated), recording the result.
pub fn run_existing_tests(
    store: &Store,
    cfg: &Config,
    p: &Problem,
    source: harness::Source,
    timeout: Duration,
    cancel: &Cancel,
) -> Result<(String, Option<(u32, u32)>)> {
    let file = harness::build_test_file(store, cfg, p, source)?;
    let output = run_captured(cfg, store, &file, timeout, cancel)?;
    let result = harness::parse_result(&output);
    if let Some((passed, total)) = result {
        harness::record(store, p, passed, total)?;
    }
    Ok((output, result))
}

fn run_command_line(cfg: &Config, store: &Store, file: &Path) -> Result<String> {
    let stem = file
        .file_stem()
        .and_then(|s| s.to_str())
        .context("bad solution path")?;
    let build = store.build_dir();
    std::fs::create_dir_all(&build)?;
    Ok(fill_placeholders(
        &cfg.run_cmd(),
        &shell_quote(file),
        &shell_quote(&build.join(stem)),
    ))
}

/// Replaces `{file}` and `{bin}` in one pass, so text inside a substituted
/// path (say a folder literally named `{bin}`) is never substituted again.
fn fill_placeholders(template: &str, file: &str, bin: &str) -> String {
    let mut out = String::with_capacity(template.len() + file.len() + bin.len());
    let mut rest = template;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        if let Some(after) = tail.strip_prefix("{file}") {
            out.push_str(file);
            rest = after;
        } else if let Some(after) = tail.strip_prefix("{bin}") {
            out.push_str(bin);
            rest = after;
        } else {
            out.push('{');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

pub(crate) fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

fn solution_command(cmdline: &str, file: &Path) -> Command {
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(cmdline)
        .current_dir(file.parent().unwrap_or(Path::new(".")));
    cmd
}

/// Runs the solution in the foreground with the terminal attached (CLI).
pub fn run_interactive(cfg: &Config, store: &Store, file: &Path) -> Result<ExitStatus> {
    if !file.exists() {
        bail!("no solution yet at {}", file.display());
    }
    let cmdline = run_command_line(cfg, store, file)?;
    eprintln!("$ {cmdline}");
    Ok(solution_command(&cmdline, file).status()?)
}

/// Runs the solution with captured output (TUI). The whole process group is
/// killed on timeout or cancellation, so builds and runaway loops can't linger.
pub fn run_captured(
    cfg: &Config,
    store: &Store,
    file: &Path,
    timeout: Duration,
    cancel: &Cancel,
) -> Result<String> {
    let cmdline = run_command_line(cfg, store, file)?;
    let mut child = solution_command(&cmdline, file)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start run command")?;
    let out_t = capture(child.stdout.take().context("no stdout")?);
    let err_t = capture(child.stderr.take().context("no stderr")?);

    let start = Instant::now();
    let outcome = loop {
        if let Some(st) = child.try_wait()? {
            break Ok(st);
        }
        let reason = if cancel.is_cancelled() {
            Some("cancelled".to_string())
        } else if start.elapsed() > timeout {
            Some(format!("killed after {}s timeout", timeout.as_secs()))
        } else {
            None
        };
        if let Some(reason) = reason {
            // Kill before reaping: the group id can't be reused yet.
            proc::kill_group(child.id());
            let _ = child.wait();
            break Err(reason);
        }
        std::thread::sleep(Duration::from_millis(30));
    };
    let elapsed = start.elapsed();
    let stdout = out_t.join().unwrap_or_default();
    let stderr = err_t.join().unwrap_or_default();

    let mut text = format!("$ {cmdline}\n\n{stdout}");
    if !stderr.is_empty() {
        if !stdout.is_empty() && !stdout.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&stderr);
    }
    let footer = match outcome {
        Ok(st) if st.success() => format!("ok in {:.2}s", elapsed.as_secs_f64()),
        Ok(st) => format!("failed: {st} after {:.2}s", elapsed.as_secs_f64()),
        Err(reason) => reason,
    };
    text.push_str(&format!("\n[{footer}]"));
    Ok(text)
}

/// Reads a pipe to the end on a thread, keeping at most `MAX_OUTPUT` bytes.
fn capture(mut pipe: impl Read + Send + 'static) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = (&mut pipe).take(MAX_OUTPUT).read_to_end(&mut buf);
        let rest = std::io::copy(&mut pipe, &mut std::io::sink()).unwrap_or(0);
        let mut s = String::from_utf8_lossy(&buf).into_owned();
        if rest > 0 {
            s.push_str(&format!("\n[... {rest} more bytes not shown]\n"));
        }
        s
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn placeholders_are_filled_once() {
        assert_eq!(
            fill_placeholders("g++ -o {bin} {file} && {bin}", "'/a/{bin}/x.cpp'", "'/b/x'"),
            "g++ -o '/b/x' '/a/{bin}/x.cpp' && '/b/x'"
        );
        assert_eq!(
            fill_placeholders("echo {x} {file", "F", "B"),
            "echo {x} {file"
        );
    }

    #[test]
    fn shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote(Path::new("/a/it's")), "'/a/it'\\''s'");
    }

    /// A store in a fresh temp folder, and a config whose Python run command is `cmd`.
    fn run_sandbox(name: &str, cmd: &str) -> (Store, Config, PathBuf) {
        let dir = std::env::temp_dir().join(format!("neet-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store {
            root: dir.join("data"),
            workspace: dir.join("workspace"),
        };
        let mut cfg = Config {
            language: "python".into(),
            ..Config::default()
        };
        cfg.run
            .insert("python".into(), cmd.replace("{dir}", &shell_quote(&dir)));
        let file = dir.join("solution.py");
        std::fs::write(&file, "").unwrap();
        (store, cfg, file)
    }

    #[test]
    fn timeout_kills_the_whole_process_group() {
        // A background child that would outlive a plain kill of the shell.
        let (store, cfg, file) = run_sandbox("timeout", "(sleep 1; touch {dir}/leaked) & sleep 30");
        let start = Instant::now();
        let out = run_captured(
            &cfg,
            &store,
            &file,
            Duration::from_millis(300),
            &Cancel::default(),
        )
        .unwrap();
        assert!(start.elapsed() < Duration::from_secs(5), "{out}");
        assert!(out.contains("timeout"), "{out}");
        std::thread::sleep(Duration::from_millis(1500));
        let dir = file.parent().unwrap();
        assert!(!dir.join("leaked").exists(), "background child survived");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn output_is_capped() {
        let (store, cfg, file) = run_sandbox("cap", "head -c 1000000 /dev/zero | tr '\\0' a");
        let out = run_captured(
            &cfg,
            &store,
            &file,
            Duration::from_secs(20),
            &Cancel::default(),
        )
        .unwrap();
        assert!(
            out.len() < MAX_OUTPUT as usize + 1024,
            "{} bytes",
            out.len()
        );
        assert!(out.contains("more bytes not shown"));
        assert!(out.contains("[ok in"));
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }
}
