//! UI strings. TernaryGrid's interface is English only: [`tr`] returns its argument
//! and [`trf`] substitutes placeholders (`{}` sequential or `{0}`, `{1}`
//! positional) into an English template.

use std::fmt::Display;

/// A user visible string (English, unchanged).
#[inline]
pub fn tr(s: &str) -> &str {
    s
}

/// Substitute placeholders: `{}` takes arguments in order, `{0}`, `{1}` ...
/// select by position.
pub fn trf(s: &str, args: &[&dyn Display]) -> String {
    format_placeholders(s, args)
}

pub fn format_placeholders(template: &str, args: &[&dyn Display]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut next = 0usize;
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{'
            && let Some(close) = template[i + 1..].find('}')
        {
            let inner = &template[i + 1..i + 1 + close];
            if inner.is_empty() {
                if let Some(a) = args.get(next) {
                    out.push_str(&a.to_string());
                }
                next += 1;
                i += close + 2;
                continue;
            }
            if let Ok(n) = inner.parse::<usize>() {
                if let Some(a) = args.get(n) {
                    out.push_str(&a.to_string());
                }
                i += close + 2;
                continue;
            }
        }
        let ch = template[i..].chars().next().expect("char boundary");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders() {
        assert_eq!(format_placeholders("{} of {}", &[&1, &2]), "1 of 2");
        assert_eq!(format_placeholders("{1} before {0}", &[&"a", &"b"]), "b before a");
        assert_eq!(format_placeholders("no {x} args", &[]), "no {x} args");
    }
}
