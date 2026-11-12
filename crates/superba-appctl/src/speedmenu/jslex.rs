//! Just enough JavaScript lexing to find where a braced block ends in the
//! desktop app's minified Rspack chunks: strings, template literals (with
//! nested `${}`), comments and regular-expression literals are skipped so their
//! braces do not count.
//!
//! Whether a `/` starts a regular expression or divides is decided from the
//! previous significant token, as minifier output allows: after a value
//! (identifier, number, string, `)`, `]`) it divides; after an operator,
//! `(`, `,`, `{`, `}`, `;` or a keyword such as `return` it starts a regular
//! expression. A `)` closing the head of `if`/`for`/`while`/`with` is followed
//! by a statement, so a regular expression may follow it.
//!
//! The module table is checked against the original in the page (length and
//! hash of `Function.prototype.toString`), so a wrong guess here costs the
//! patch, never the app.

/// Keywords after which an expression (so a regular expression) may follow.
const EXPRESSION_KEYWORDS: &[&[u8]] = &[
    b"return",
    b"typeof",
    b"instanceof",
    b"in",
    b"of",
    b"new",
    b"delete",
    b"void",
    b"throw",
    b"case",
    b"do",
    b"else",
    b"yield",
    b"await",
];

/// Keywords whose parenthesised head is followed by a statement.
const CONTROL_KEYWORDS: &[&[u8]] = &[b"if", b"for", b"while", b"with"];

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b >= 0x80
}

struct Lexer<'a> {
    src: &'a [u8],
    pos: usize,
    /// A `/` here starts a regular expression.
    regex_ok: bool,
    /// The previous significant token was `.` or `?.` (the next word is a
    /// property name, never a keyword).
    after_dot: bool,
}

impl<'a> Lexer<'a> {
    fn peek(&self, offset: usize) -> Option<u8> {
        self.src.get(self.pos + offset).copied()
    }

    /// Skip a quoted string starting at `self.pos` (the quote).
    fn string(&mut self, quote: u8) -> Option<()> {
        self.pos += 1;
        loop {
            match self.peek(0)? {
                b'\\' => self.pos += 2,
                b'\n' | b'\r' => return None,
                b if b == quote => {
                    self.pos += 1;
                    return Some(());
                }
                _ => self.pos += 1,
            }
        }
    }

    /// Skip a template literal starting at `self.pos` (the backtick).
    fn template(&mut self) -> Option<()> {
        self.pos += 1;
        loop {
            match self.peek(0)? {
                b'\\' => self.pos += 2,
                b'`' => {
                    self.pos += 1;
                    return Some(());
                }
                b'$' if self.peek(1) == Some(b'{') => {
                    self.pos += 2;
                    self.regex_ok = true;
                    self.after_dot = false;
                    self.code()?;
                    // `code` stops on the `}` closing the substitution.
                    self.pos += 1;
                }
                _ => self.pos += 1,
            }
        }
    }

    /// Skip a regular-expression literal starting at `self.pos` (the slash).
    fn regex(&mut self) -> Option<()> {
        self.pos += 1;
        let mut class = false;
        loop {
            match self.peek(0)? {
                b'\\' => self.pos += 2,
                b'\n' | b'\r' => return None,
                b'[' => {
                    class = true;
                    self.pos += 1;
                }
                b']' => {
                    class = false;
                    self.pos += 1;
                }
                b'/' if !class => {
                    self.pos += 1;
                    while self.peek(0).is_some_and(is_ident_byte) {
                        self.pos += 1;
                    }
                    return Some(());
                }
                _ => self.pos += 1,
            }
        }
    }

    /// Scan code until the `}` that closes the current block; `self.pos` is
    /// left on it.
    fn code(&mut self) -> Option<()> {
        let mut depth = 0usize;
        // One entry per open `(`: whether it is a control-flow head.
        let mut parens: Vec<bool> = Vec::new();
        let mut last_word: &[u8] = b"";
        loop {
            let b = self.peek(0)?;
            match b {
                b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c => {
                    self.pos += 1;
                    continue;
                }
                b'/' if self.peek(1) == Some(b'/') => {
                    while self.peek(0).is_some_and(|c| c != b'\n') {
                        self.pos += 1;
                    }
                    continue;
                }
                b'/' if self.peek(1) == Some(b'*') => {
                    let rest = &self.src[self.pos + 2..];
                    let end = memchr::memmem::find(rest, b"*/")?;
                    self.pos += 2 + end + 2;
                    continue;
                }
                _ => {}
            }
            let word_before = std::mem::take(&mut last_word);
            let after_dot = std::mem::replace(&mut self.after_dot, false);
            match b {
                b'\'' | b'"' => {
                    self.string(b)?;
                    self.regex_ok = false;
                }
                b'`' => {
                    self.template()?;
                    self.regex_ok = false;
                }
                b'/' => {
                    if self.regex_ok {
                        self.regex()?;
                        self.regex_ok = false;
                    } else {
                        self.pos += 1;
                        if self.peek(0) == Some(b'=') {
                            self.pos += 1;
                        }
                        self.regex_ok = true;
                    }
                }
                b'{' => {
                    depth += 1;
                    self.pos += 1;
                    self.regex_ok = true;
                }
                b'}' => {
                    if depth == 0 {
                        return Some(());
                    }
                    depth -= 1;
                    self.pos += 1;
                    self.regex_ok = true;
                }
                b'(' => {
                    parens.push(CONTROL_KEYWORDS.contains(&word_before) && !after_dot);
                    self.pos += 1;
                    self.regex_ok = true;
                }
                b')' => {
                    self.regex_ok = parens.pop().unwrap_or(false);
                    self.pos += 1;
                }
                b']' => {
                    self.pos += 1;
                    self.regex_ok = false;
                }
                b'.' if self.peek(1).is_some_and(|c| c.is_ascii_digit()) => {
                    self.number();
                }
                b'.' => {
                    self.pos += 1;
                    self.after_dot = true;
                    self.regex_ok = false;
                }
                b'?' if self.peek(1) == Some(b'.')
                    && !self.peek(2).is_some_and(|c| c.is_ascii_digit()) =>
                {
                    self.pos += 2;
                    self.after_dot = true;
                    self.regex_ok = false;
                }
                b'+' | b'-' if self.peek(1) == Some(b) => {
                    self.pos += 2;
                    self.regex_ok = false;
                }
                b'0'..=b'9' => self.number(),
                b'#' => {
                    self.pos += 1;
                    while self.peek(0).is_some_and(is_ident_byte) {
                        self.pos += 1;
                    }
                    self.regex_ok = false;
                }
                b if is_ident_byte(b) => {
                    let start = self.pos;
                    while self.peek(0).is_some_and(is_ident_byte) {
                        self.pos += 1;
                    }
                    let word = &self.src[start..self.pos];
                    let keyword = !after_dot;
                    self.regex_ok = keyword && EXPRESSION_KEYWORDS.contains(&word);
                    if keyword {
                        last_word = word;
                    }
                }
                _ => {
                    // Any other punctuator: an operand may follow.
                    self.pos += 1;
                    self.regex_ok = true;
                }
            }
        }
    }

    fn number(&mut self) {
        let start = self.pos;
        self.pos += 1;
        while let Some(c) = self.peek(0) {
            let hex = matches!(&self.src[start..], [b'0', b'x' | b'X', ..]);
            let exponent_sign = (c == b'+' || c == b'-')
                && matches!(self.src.get(self.pos - 1), Some(b'e' | b'E'))
                && !hex;
            if c.is_ascii_alphanumeric() || c == b'.' || c == b'_' || exponent_sign {
                self.pos += 1;
            } else {
                break;
            }
        }
        self.regex_ok = false;
    }
}

/// The index of the `}` that closes the `{` at `open`, or `None` when the text
/// ends first or contains something this lexer does not understand.
pub fn matching_brace(src: &[u8], open: usize) -> Option<usize> {
    if src.get(open) != Some(&b'{') {
        return None;
    }
    let mut lexer = Lexer {
        src,
        pos: open + 1,
        regex_ok: true,
        after_dot: false,
    };
    lexer.code()?;
    Some(lexer.pos)
}

/// The index of the `)` that closes the `(` at `open` (a parameter list).
pub fn matching_paren(src: &[u8], open: usize) -> Option<usize> {
    if src.get(open) != Some(&b'(') {
        return None;
    }
    // Parameter lists of module factories are plain identifiers; anything
    // else (defaults, destructuring) is left to the brace scanner by wrapping.
    let mut depth = 0usize;
    for (i, &b) in src.iter().enumerate().skip(open) {
        match b {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            b'{' | b'}' | b'\'' | b'"' | b'`' | b'/' => return None,
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn end(src: &str) -> Option<usize> {
        matching_brace(src.as_bytes(), 0)
    }

    #[test]
    fn nested_blocks() {
        assert_eq!(end("{a{b}c}d"), Some(6));
        assert_eq!(end("{a{b}c"), None);
    }

    #[test]
    fn braces_in_strings_templates_and_comments_do_not_count() {
        assert_eq!(end(r#"{"}"+'{'+"\"}"}x"#), Some(14));
        assert_eq!(end("{`}${a+`${'}'}`}{`}x"), Some(18));
        assert_eq!(end("{/*}*/a//}\n}x"), Some(11));
    }

    #[test]
    fn regex_after_operators_and_keywords_division_after_values() {
        assert_eq!(end("{x=/}/.test(y)}z"), Some(14));
        assert_eq!(end("{return/[}/]/g}z"), Some(14));
        assert_eq!(end("{a=b/c/d}z"), Some(8));
        assert_eq!(end("{a=(b)/2/c}z"), Some(10));
        assert_eq!(end("{if(a)/}/.test(b)}z"), Some(17));
        assert_eq!(end("{a=x.return/2/c}z"), Some(15));
    }

    #[test]
    fn numbers_and_optional_chaining() {
        assert_eq!(end("{a=1e-5/2/3,b=a?.c/2/1,c=x?.5:1}z"), Some(31));
    }

    #[test]
    fn parameter_lists() {
        assert_eq!(matching_paren(b"(e,t,n){", 0), Some(6));
        assert_eq!(matching_paren(b"(e={}){", 0), None);
    }
}
