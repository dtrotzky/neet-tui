//! Tiny line-based syntax highlighter for code blocks: keywords, strings,
//! numbers and comments. Good enough for reading solutions, no dependencies.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

const PYTHON: &[&str] = &[
    "and", "as", "assert", "break", "class", "continue", "def", "del", "elif", "else", "except",
    "False", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "None",
    "nonlocal", "not", "or", "pass", "raise", "return", "self", "True", "try", "while", "with",
    "yield",
];

const C_LIKE: &[&str] = &[
    "auto",
    "bool",
    "break",
    "case",
    "catch",
    "char",
    "class",
    "const",
    "continue",
    "default",
    "delete",
    "do",
    "double",
    "else",
    "enum",
    "false",
    "final",
    "float",
    "for",
    "fun",
    "func",
    "if",
    "impl",
    "import",
    "in",
    "int",
    "interface",
    "let",
    "long",
    "loop",
    "match",
    "mut",
    "new",
    "nil",
    "null",
    "nullptr",
    "package",
    "private",
    "public",
    "return",
    "self",
    "Self",
    "static",
    "string",
    "struct",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "type",
    "typename",
    "unsigned",
    "use",
    "using",
    "val",
    "var",
    "vector",
    "void",
    "while",
    "fn",
    "pub",
    "range",
    "map",
    "go",
    "defer",
    "function",
    "of",
    "extends",
    "implements",
    "namespace",
    "template",
    "export",
    "async",
    "await",
    "where",
    "as",
    "dyn",
    "ref",
    "None",
    "Some",
];

pub fn highlight(line: &str, lang: &str, base: Style) -> Vec<Span<'static>> {
    let python = matches!(lang, "python" | "py" | "python3");
    let comment: Vec<char> = if python { "#" } else { "//" }.chars().collect();
    let keywords = if python { PYTHON } else { C_LIKE };
    let kw = base.fg(Color::Magenta).add_modifier(Modifier::BOLD);
    let string = base.fg(Color::Green);
    let number = base.fg(Color::Cyan);
    let comment_style = base.fg(Color::DarkGray).add_modifier(Modifier::ITALIC);

    let mut spans = Vec::new();
    let mut plain = String::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    let flush = |plain: &mut String, spans: &mut Vec<Span<'static>>| {
        if !plain.is_empty() {
            spans.push(Span::styled(std::mem::take(plain), base));
        }
    };
    while i < chars.len() {
        let c = chars[i];
        if chars[i..].starts_with(&comment) {
            flush(&mut plain, &mut spans);
            spans.push(Span::styled(
                chars[i..].iter().collect::<String>(),
                comment_style,
            ));
            return spans;
        }
        if c == '"' || c == '\'' || c == '`' {
            flush(&mut plain, &mut spans);
            let mut j = i + 1;
            while j < chars.len() && chars[j] != c {
                if chars[j] == '\\' {
                    j += 1;
                }
                j += 1;
            }
            let end = (j + 1).min(chars.len());
            spans.push(Span::styled(
                chars[i..end].iter().collect::<String>(),
                string,
            ));
            i = end;
            continue;
        }
        if c.is_alphanumeric() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            // Only highlight whole tokens (not the "in" inside "index").
            let style = if word.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                Some(number)
            } else if keywords.contains(&word.as_str()) {
                Some(kw)
            } else {
                None
            };
            match style {
                Some(st) => {
                    flush(&mut plain, &mut spans);
                    spans.push(Span::styled(word, st));
                }
                None => plain.push_str(&word),
            }
            continue;
        }
        plain.push(c);
        i += 1;
    }
    flush(&mut plain, &mut spans);
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(spans: &[Span]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn keeps_text_intact() {
        let line = r#"    for i in range(len(nums)):  # "loop" 42"#;
        let spans = highlight(line, "python", Style::default());
        assert_eq!(text(&spans), line);
        assert!(spans.iter().any(|s| s.content == "for"));
        assert!(spans.iter().any(|s| s.content.starts_with("# ")));
    }

    #[test]
    fn handles_unterminated_strings_and_unicode() {
        let line = "let s = \"héllo";
        assert_eq!(text(&highlight(line, "rust", Style::default())), line);
        let line = "x = 'a\\'b' // ünïcode";
        assert_eq!(text(&highlight(line, "cpp", Style::default())), line);
    }
}
