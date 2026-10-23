//! Finding problems: by name, number or file, and picking the next or a random one.

use std::path::Path;

use anyhow::{Result, bail};

use crate::model::store::{Problem, Progress, Status, Store};

/// Finds a problem by LeetCode/NeetCode slug, file stem, LeetCode number,
/// title, or a solution file path.
pub fn resolve<'a>(problems: &'a [Problem], store: &Store, query: &str) -> Result<&'a Problem> {
    let path = Path::new(query);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or(query);
    let q = stem.trim().to_lowercase();
    let found = problems.iter().find(|p| {
        p.slug == q || p.nc_slug == q || p.file_stem() == q || p.title.to_lowercase() == q
    });
    if let Some(p) = found {
        return Ok(p);
    }
    if !q.is_empty() && q.bytes().all(|b| b.is_ascii_digit()) {
        let num = q.trim_start_matches('0');
        if let Some(p) = problems.iter().find(|p| {
            store
                .load_details(&p.slug)
                .and_then(|d| d.id)
                .is_some_and(|id| id == num)
        }) {
            return Ok(p);
        }
    }
    let fuzzy: Vec<&Problem> = problems
        .iter()
        .filter(|p| p.title.to_lowercase().contains(&q) || p.slug.contains(&q))
        .collect();
    match fuzzy.as_slice() {
        [one] => Ok(one),
        [] => bail!("no problem matches '{query}'"),
        many => bail!(
            "'{query}' is ambiguous: {}",
            many.iter()
                .take(8)
                .map(|p| p.slug.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn unsolved(progress: &Progress, p: &Problem) -> bool {
    progress
        .entries
        .get(&p.slug)
        .is_none_or(|e| e.status != Status::Solved)
}

/// The next unsolved problem in roadmap order after `after`, wrapping around.
pub fn next_unsolved(
    problems: &[Problem],
    progress: &Progress,
    include: impl Fn(&Problem) -> bool,
    after: Option<usize>,
) -> Option<usize> {
    let n = problems.len();
    let start = after.map_or(0, |a| a + 1);
    (0..n)
        .map(|k| (start + k) % n)
        .find(|&i| include(&problems[i]) && unsolved(progress, &problems[i]))
}

/// A random unsolved problem.
pub fn random_unsolved(
    problems: &[Problem],
    progress: &Progress,
    include: impl Fn(&Problem) -> bool,
) -> Option<usize> {
    let candidates: Vec<usize> = (0..problems.len())
        .filter(|&i| include(&problems[i]) && unsolved(progress, &problems[i]))
        .collect();
    if candidates.is_empty() {
        return None;
    }
    Some(candidates[random_below(candidates.len())])
}

/// Good-enough randomness for picking a problem, without a dependency.
fn random_below(n: usize) -> usize {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut x = (nanos ^ (u64::from(std::process::id()) << 32)) | 1;
    // xorshift64*
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    (x.wrapping_mul(0x2545_F491_4F6C_DD1D) % n as u64) as usize
}
