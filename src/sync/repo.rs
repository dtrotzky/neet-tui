//! NeetCode's solution write-ups and per-language solutions, from the neetcode-gh/leetcode
//! tarball.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::time::Duration;

use anyhow::Result;

use super::get;
use crate::model::lang::LANGS;
use crate::model::store::{Problem, Store};
use crate::render::sanitize;

const TARBALL_URL: &str = "https://codeload.github.com/neetcode-gh/leetcode/tar.gz/refs/heads/main";

/// Upper bound for a single file extracted from the repo tarball.
const MAX_ENTRY: u64 = 4 * 1024 * 1024;

pub(super) fn fetch_repo(store: &Store, problems: &[Problem]) -> Result<(usize, usize)> {
    extract_repo(store, problems, get(TARBALL_URL, Duration::from_secs(600))?)
}

/// Saves the articles and solution files for known problems from the repo's `.tar.gz`.
/// Destinations come from the problem list, never from names inside the archive.
fn extract_repo(store: &Store, problems: &[Problem], tgz: impl Read) -> Result<(usize, usize)> {
    let mut by_article: HashMap<&str, &str> = HashMap::new();
    let mut by_code: HashMap<&str, &str> = HashMap::new();
    for p in problems {
        by_article.insert(&p.nc_slug, &p.slug);
        by_code.insert(p.code.as_deref().unwrap_or(&p.slug), &p.slug);
    }
    // LeetCode-slug-named articles are used only when no NeetCode-named one exists.
    let mut fallback_articles: HashMap<&str, &str> = HashMap::new();
    for p in problems {
        if !by_article.contains_key(p.slug.as_str()) {
            fallback_articles.insert(&p.slug, &p.slug);
        }
    }
    let lang_dirs: HashSet<&str> = LANGS.iter().map(|l| l.repo_dir).collect();
    let mut have_article: HashSet<String> = HashSet::new();
    let (mut articles, mut codes) = (0, 0);

    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(tgz));
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let parts: Vec<String> = path
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        // <repo-root>/<dir>/<file>
        let [_, dir, file] = parts.as_slice() else {
            continue;
        };
        let Some((stem, ext)) = file.rsplit_once('.') else {
            continue;
        };
        if dir == "articles" && ext == "md" {
            let primary = by_article.get(stem).copied();
            let slug = primary.or_else(|| fallback_articles.get(stem).copied());
            if let Some(slug) = slug {
                if primary.is_none() && have_article.contains(slug) {
                    continue;
                }
                let text = read_entry(&mut entry)?;
                let dest = store.article_path(slug);
                std::fs::create_dir_all(store.problem_dir(slug))?;
                std::fs::write(dest, text)?;
                if have_article.insert(slug.to_string()) {
                    articles += 1;
                }
            }
        } else if lang_dirs.contains(dir.as_str())
            && let Some(slug) = by_code.get(stem)
        {
            let Some(lang) = LANGS.iter().find(|l| l.repo_dir == dir) else {
                continue;
            };
            let dest = store.ref_solution_path(slug, lang);
            std::fs::create_dir_all(store.problem_dir(slug).join("solutions"))?;
            std::fs::write(dest, read_entry(&mut entry)?)?;
            codes += 1;
        }
    }
    Ok((articles, codes))
}

/// Reads a tarball entry as sanitized text (lossy UTF-8, size-capped).
fn read_entry(entry: &mut impl Read) -> Result<String> {
    let mut buf = Vec::new();
    entry.take(MAX_ENTRY).read_to_end(&mut buf)?;
    Ok(sanitize(&String::from_utf8_lossy(&buf)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::neetcode::bundled_lists;
    use crate::sync::tests::temp_store;

    fn tgz(files: &[(&str, &str)]) -> Vec<u8> {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut tar = tar::Builder::new(gz);
        for (path, body) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, path, body.as_bytes()).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn extracts_articles_and_solutions_for_known_problems() {
        let store = temp_store("repo");
        let problems = bundled_lists().unwrap();
        let archive = tgz(&[
            // NeetCode-named article wins over the LeetCode-named one.
            (
                "leetcode-main/articles/duplicate-integer.md",
                "# Hash set\x1b[31m\n",
            ),
            (
                "leetcode-main/articles/contains-duplicate.md",
                "older copy\n",
            ),
            (
                "leetcode-main/python/0217-contains-duplicate.py",
                "class Solution: ...\n",
            ),
            ("leetcode-main/python/9999-not-in-the-list.py", "ignored\n"),
            ("leetcode-main/articles/nested/two-sum.md", "ignored\n"),
            ("leetcode-main/README.md", "ignored\n"),
        ]);
        let counts = extract_repo(&store, &problems, archive.as_slice()).unwrap();
        assert_eq!(counts, (1, 1));
        let article = store.load_article("contains-duplicate").unwrap();
        assert!(
            article.starts_with("# Hash set") && !article.contains('\x1b'),
            "{article:?}"
        );
        let python = LANGS.iter().find(|l| l.key == "python").unwrap();
        assert!(
            store
                .load_ref_solution("contains-duplicate", python)
                .is_some()
        );
        assert!(store.load_article("two-sum").is_none());
        let _ = std::fs::remove_dir_all(&store.root);
    }
}
