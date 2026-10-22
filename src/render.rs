//! Minimal Markdown -> ratatui rendering, plus NeetCode article filtering.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::lang::Lang;

/// Strips control characters (terminal escape sequences in particular) from
/// downloaded or model-generated text before it is stored or printed raw.
pub fn sanitize(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect()
}

/// NeetCode articles contain every language inside `::tabs-start` blocks.
/// Keep only the user's language (falling back to Python, then the first block).
pub fn filter_article(article: &str, lang: &Lang) -> String {
    let mut out = String::new();
    let mut in_tabs = false;
    let mut blocks: Vec<(String, String)> = Vec::new();
    let mut current: Option<(String, String)> = None;

    for line in article.lines() {
        let trimmed = line.trim();
        if !in_tabs {
            if trimmed == "::tabs-start" {
                in_tabs = true;
                blocks.clear();
            } else {
                out.push_str(line);
                out.push('\n');
            }
            continue;
        }
        if let Some((fence, body)) = current.as_mut() {
            if trimmed.starts_with("```") {
                blocks.push((std::mem::take(fence), std::mem::take(body)));
                current = None;
            } else {
                body.push_str(line);
                body.push('\n');
            }
        } else if let Some(fence) = trimmed.strip_prefix("```") {
            current = Some((fence.trim().to_lowercase(), String::new()));
        } else if trimmed == "::tabs-end" {
            in_tabs = false;
            let pick = blocks
                .iter()
                .find(|(f, _)| lang.fences.contains(&f.as_str()))
                .or_else(|| blocks.iter().find(|(f, _)| f == "python"))
                .or_else(|| blocks.first());
            if let Some((fence, body)) = pick {
                out.push_str(&format!("```{fence}\n{body}```\n"));
            }
        }
    }
    out
}

pub fn markdown(text: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut in_code = false;
    let mut code_lang = String::new();
    let code_style = Style::default().fg(Color::Rgb(220, 200, 150));
    let gutter = Style::default().fg(Color::DarkGray);

    for raw in text.lines() {
        let trimmed = raw.trim_start();
        if trimmed.starts_with("```") {
            in_code = !in_code;
            let label = trimmed.trim_start_matches('`').trim();
            code_lang = label.to_lowercase();
            if in_code && !label.is_empty() {
                lines.push(Line::from(Span::styled(format!("  ╭─ {label}"), gutter)));
            } else if in_code {
                lines.push(Line::from(Span::styled("  ╭─", gutter)));
            } else {
                lines.push(Line::from(Span::styled("  ╰─", gutter)));
            }
            continue;
        }
        if in_code {
            let mut spans = vec![Span::styled("  │ ", gutter)];
            let code = raw.replace('\t', "    ");
            if code_lang.is_empty() || code_lang == "text" {
                spans.push(Span::styled(code, code_style));
            } else {
                spans.extend(crate::highlight::highlight(&code, &code_lang, code_style));
            }
            lines.push(Line::from(spans));
            continue;
        }
        if let Some(h) = raw.strip_prefix("# ") {
            lines.push(Line::from(Span::styled(
                h.to_string(),
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            )));
        } else if let Some(h) = raw.strip_prefix("## ") {
            lines.push(Line::from(Span::styled(
                h.to_string(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
        } else if let Some(h) = raw
            .strip_prefix("### ")
            .or_else(|| raw.strip_prefix("#### "))
        {
            lines.push(Line::from(Span::styled(
                h.to_string(),
                Style::default()
                    .fg(Color::Blue)
                    .add_modifier(Modifier::BOLD),
            )));
        } else if matches!(raw.trim(), "---" | "***" | "___") {
            lines.push(Line::from(Span::styled("─".repeat(48), gutter)));
        } else if let Some(q) = raw.strip_prefix("> ") {
            let mut spans = vec![Span::styled("▎ ", gutter)];
            spans.extend(inline(q, Style::default().add_modifier(Modifier::ITALIC)));
            lines.push(Line::from(spans));
        } else if raw.trim() == "::tabs-start" || raw.trim() == "::tabs-end" {
            continue;
        } else {
            let indent = raw.len() - trimmed.len();
            let (prefix, rest) = if let Some(r) = trimmed
                .strip_prefix("- ")
                .or_else(|| trimmed.strip_prefix("* "))
            {
                (format!("{}• ", " ".repeat(indent)), r)
            } else {
                (" ".repeat(indent), trimmed)
            };
            let mut spans = vec![Span::raw(prefix)];
            spans.extend(inline(rest, Style::default()));
            lines.push(Line::from(spans));
        }
    }
    lines
}

/// Handles `**bold**`, `*italic*` and `` `code` `` spans.
fn inline(text: &str, base: Style) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let mut spans = Vec::new();
    let mut buf = String::new();
    let (mut bold, mut italic, mut code) = (false, false, false);
    let style = |bold: bool, italic: bool, code: bool| {
        let mut s = base;
        if bold {
            s = s.add_modifier(Modifier::BOLD);
        }
        if italic {
            s = s.add_modifier(Modifier::ITALIC);
        }
        if code {
            s = s.fg(Color::Yellow);
        }
        s
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c == '`' {
            spans.push(Span::styled(
                std::mem::take(&mut buf),
                style(bold, italic, code),
            ));
            code = !code;
        } else if c == '*' && !code && next == Some('*') {
            spans.push(Span::styled(
                std::mem::take(&mut buf),
                style(bold, italic, code),
            ));
            bold = !bold;
            i += 1;
        } else if c == '*' && !code && is_italic_marker(&chars, i, italic) {
            spans.push(Span::styled(
                std::mem::take(&mut buf),
                style(bold, italic, code),
            ));
            italic = !italic;
        } else {
            buf.push(c);
        }
        i += 1;
    }
    spans.push(Span::styled(buf, style(bold, italic, code)));
    spans.retain(|s| !s.content.is_empty());
    spans
}

/// A single `*` opens italics only when followed by text and closed later on
/// the line, so arithmetic like `2 * n` stays literal.
fn is_italic_marker(chars: &[char], i: usize, open: bool) -> bool {
    let prev = i.checked_sub(1).map(|p| chars[p]);
    let next = chars.get(i + 1).copied();
    if open {
        return prev.is_some_and(|c| !c.is_whitespace());
    }
    next.is_some_and(|c| !c.is_whitespace() && c != '*')
        && chars[i + 1..]
            .windows(2)
            .any(|w| w[1] == '*' && !w[0].is_whitespace() && w[0] != '*')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang;

    #[test]
    fn sanitize_strips_escape_sequences() {
        assert_eq!(sanitize("a\x1b[31mb\x07\tc\nd\r"), "a[31mb\tc\nd");
    }

    #[test]
    fn inline_styles() {
        let text = |spans: &[Span]| {
            spans
                .iter()
                .map(|s| s.content.to_string())
                .collect::<Vec<_>>()
        };
        let spans = inline("a *b* c **d** `e`", Style::default());
        assert_eq!(text(&spans), ["a ", "b", " c ", "d", " ", "e"]);
        assert!(spans[1].style.add_modifier.contains(Modifier::ITALIC));
        // Arithmetic is left alone.
        assert_eq!(text(&inline("2 * n * m", Style::default())), ["2 * n * m"]);
    }

    #[test]
    fn filter_article_keeps_one_language() {
        let article =
            "intro\n::tabs-start\n```python\npy\n```\n```cpp\ncc\n```\n::tabs-end\nouttro\n";
        let cpp = lang::by_key("cpp").unwrap();
        assert_eq!(
            filter_article(article, cpp),
            "intro\n```cpp\ncc\n```\nouttro\n"
        );
        // Falls back to Python when the language is missing.
        let go = lang::by_key("go").unwrap();
        assert_eq!(
            filter_article(article, go),
            "intro\n```python\npy\n```\nouttro\n"
        );
    }
}
