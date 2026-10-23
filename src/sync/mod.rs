//! One-time download of everything needed to practice offline:
//! the NeetCode problem lists, NeetCode solution articles and code
//! (from the neetcode-gh/leetcode repo), and LeetCode problem statements.

use std::io::Read;
use std::sync::LazyLock;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::config::Config;
use crate::model::store::{Problem, Store};

mod leetcode;
mod neetcode;
mod premium;
mod repo;

use leetcode::fetch_descriptions;
use neetcode::{bundled_lists, fetch_lists_from_site};
use premium::{write_premium_starters, write_premium_statements};
use repo::fetch_repo;

const USER_AGENT: &str = "neet (offline NeetCode practice; https://github.com/dtrotzky/neet-tui)";
/// Upper bound for any single downloaded document (HTML, JS bundle, JSON).
const MAX_DOC: u64 = 32 * 1024 * 1024;

static AGENT: LazyLock<ureq::Agent> = LazyLock::new(|| {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_global(Some(Duration::from_secs(60)))
        .user_agent(USER_AGENT)
        .build()
        .into()
});

pub fn run(store: &Store, cfg: &Config, force: bool) -> Result<()> {
    println!("==> Fetching NeetCode problem lists");
    let problems = match fetch_lists_from_site() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("    neetcode.io lists unavailable ({e:#})");
            match store.load_problems() {
                // An earlier sync is usually newer than the bundled snapshot.
                Ok(cached) if cached.iter().any(|p| p.nc250) => {
                    eprintln!("    keeping the cached problem list");
                    cached
                }
                _ => {
                    eprintln!("    using the problem list bundled with neet");
                    bundled_lists()?
                }
            }
        }
    };
    let count = |f: fn(&Problem) -> bool| problems.iter().filter(|p| f(p)).count();
    println!(
        "    {} problems (Blind 75: {}, NeetCode 150: {}, NeetCode 250: {})",
        problems.len(),
        count(|p| p.blind75),
        count(|p| p.nc150),
        count(|p| p.nc250),
    );
    store.save_problems(&problems)?;

    println!("==> Downloading NeetCode solutions (neetcode-gh/leetcode)");
    let (articles, codes) = fetch_repo(store, &problems)?;
    println!("    {articles} solution articles, {codes} solution files");

    println!("==> Fetching problem statements from LeetCode");
    fetch_descriptions(store, &problems, force)?;
    write_premium_statements(store, cfg, &problems)?;
    write_premium_starters(store, cfg, &problems)?;

    println!("==> Done. Data stored in {}", store.root.display());
    Ok(())
}

fn get(url: &str, timeout: Duration) -> Result<impl Read> {
    let resp = AGENT
        .get(url)
        .config()
        .timeout_global(Some(timeout))
        .build()
        .call()
        .with_context(|| format!("GET {url}"))?;
    Ok(resp.into_body().into_reader())
}

fn get_string(url: &str) -> Result<String> {
    let mut buf = Vec::new();
    get(url, Duration::from_secs(60))?
        .take(MAX_DOC)
        .read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn temp_store(name: &str) -> Store {
        let dir = std::env::temp_dir().join(format!("neet-sync-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Store {
            root: dir.clone(),
            workspace: dir.join("workspace"),
        }
    }

    pub(super) fn two_sum() -> Problem {
        bundled_lists()
            .unwrap()
            .into_iter()
            .find(|p| p.slug == "two-sum")
            .unwrap()
    }
}
