//! Streaming byte-slice lexer for Synopsys Liberty (`.lib`) files.
//!
//! Tracks exact 1-based line numbers across comments, quoted strings, and
//! backslash line continuations, and provides an O(bytes) brace-depth skipper
//! for timing/power NLDM/CCS lookup tables so multi-hundred-MB foundry `.lib`
//! files stream without AST bloat.

use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum LibertyError {
    #[error("read_liberty: I/O error on `{file}`: {message}")]
    Io { file: String, message: String },
    #[error("read_liberty: syntax error at {file}:{line}: {message}")]
    Syntax {
        file: String,
        line: usize,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokKind<'a> {
    LParen,
    RParen,
    LBrace,
    RBrace,
    Colon,
    Semi,
    Comma,
    Quoted(String),
    Atom(&'a str),
    Eof,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tok<'a> {
    pub kind: TokKind<'a>,
    pub line: usize,
}

pub struct Lexer<'a> {
    file: String,
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    line: usize,
    peeked: Option<Tok<'a>>,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str, file: impl Into<String>) -> Self {
        Self {
            file: file.into(),
            src,
            bytes: src.as_bytes(),
            pos: 0,
            line: 1,
            peeked: None,
        }
    }

    pub fn file(&self) -> &str {
        &self.file
    }

    pub fn line(&self) -> usize {
        self.peeked.as_ref().map_or(self.line, |t| t.line)
    }

    pub fn err(&self, line: usize, message: impl Into<String>) -> LibertyError {
        LibertyError::Syntax {
            file: self.file.clone(),
            line,
            message: message.into(),
        }
    }

    pub fn peek(&mut self) -> Result<&Tok<'a>, LibertyError> {
        if self.peeked.is_none() {
            let tok = self.scan_token()?;
            self.peeked = Some(tok);
        }
        Ok(self.peeked.as_ref().unwrap())
    }

    pub fn next_tok(&mut self) -> Result<Tok<'a>, LibertyError> {
        if let Some(tok) = self.peeked.take() {
            return Ok(tok);
        }
        self.scan_token()
    }

    pub fn expect_lparen(&mut self) -> Result<usize, LibertyError> {
        let tok = self.next_tok()?;
        match tok.kind {
            TokKind::LParen => Ok(tok.line),
            other => Err(self.err(tok.line, format!("expected `(`, found {}", tok_desc(&other)))),
        }
    }

    pub fn expect_rparen(&mut self) -> Result<usize, LibertyError> {
        let tok = self.next_tok()?;
        match tok.kind {
            TokKind::RParen => Ok(tok.line),
            other => Err(self.err(tok.line, format!("expected `)`, found {}", tok_desc(&other)))),
        }
    }

    pub fn expect_lbrace(&mut self) -> Result<usize, LibertyError> {
        let tok = self.next_tok()?;
        match tok.kind {
            TokKind::LBrace => Ok(tok.line),
            other => Err(self.err(tok.line, format!("expected `{{`, found {}", tok_desc(&other)))),
        }
    }

    /// Skips a `{ ... }` group body starting immediately AFTER the opening `{`
    /// has been consumed (or when `{` is the next token).
    pub fn skip_braced_body(&mut self, open_line: usize) -> Result<(), LibertyError> {
        // Make sure no peeked token is buffered when entering fast byte skip.
        let mut depth: usize = 1;
        if let Some(tok) = self.peeked.take() {
            match tok.kind {
                TokKind::LBrace => depth += 1,
                TokKind::RBrace => return Ok(()),
                TokKind::Eof => {
                    return Err(self.err(open_line, "unclosed `{` before end of file"));
                }
                _ => {}
            }
        }
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            match b {
                b'\n' => {
                    self.line += 1;
                    self.pos += 1;
                }
                b'/' if self.pos + 1 < self.bytes.len() && self.bytes[self.pos + 1] == b'*' => {
                    self.skip_block_comment()?;
                }
                b'/' if self.pos + 1 < self.bytes.len() && self.bytes[self.pos + 1] == b'/' => {
                    self.skip_line_comment();
                }
                b'"' => {
                    self.skip_string_fast()?;
                }
                b'{' => {
                    depth += 1;
                    self.pos += 1;
                }
                b'}' => {
                    depth -= 1;
                    self.pos += 1;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                _ => {
                    self.pos += 1;
                }
            }
        }
        Err(self.err(open_line, "unclosed `{` before end of file"))
    }

    fn skip_ws_and_comments(&mut self) -> Result<(), LibertyError> {
        loop {
            while self.pos < self.bytes.len() {
                match self.bytes[self.pos] {
                    b' ' | b'\t' | b'\r' | 0x0b | 0x0c => self.pos += 1,
                    b'\n' => {
                        self.line += 1;
                        self.pos += 1;
                    }
                    b'\\' => {
                        // Line continuation: `\ \r?\n`
                        let mut k = self.pos + 1;
                        while k < self.bytes.len()
                            && matches!(self.bytes[k], b' ' | b'\t' | b'\r')
                        {
                            k += 1;
                        }
                        if k < self.bytes.len() && self.bytes[k] == b'\n' {
                            self.line += 1;
                            self.pos = k + 1;
                        } else {
                            break;
                        }
                    }
                    _ => break,
                }
            }
            if self.pos + 1 < self.bytes.len() && self.bytes[self.pos] == b'/' {
                if self.bytes[self.pos + 1] == b'*' {
                    self.skip_block_comment()?;
                    continue;
                }
                if self.bytes[self.pos + 1] == b'/' {
                    self.skip_line_comment();
                    continue;
                }
            }
            break;
        }
        Ok(())
    }

    fn skip_line_comment(&mut self) {
        self.pos += 2;
        while self.pos < self.bytes.len() && self.bytes[self.pos] != b'\n' {
            self.pos += 1;
        }
    }

    fn skip_block_comment(&mut self) -> Result<(), LibertyError> {
        let start_line = self.line;
        self.pos += 2;
        while self.pos + 1 < self.bytes.len() {
            if self.bytes[self.pos] == b'*' && self.bytes[self.pos + 1] == b'/' {
                self.pos += 2;
                return Ok(());
            }
            if self.bytes[self.pos] == b'\n' {
                self.line += 1;
            }
            self.pos += 1;
        }
        Err(self.err(start_line, "unterminated `/* ... */` block comment"))
    }

    fn skip_string_fast(&mut self) -> Result<(), LibertyError> {
        let start_line = self.line;
        self.pos += 1; // opening `"`
        while self.pos < self.bytes.len() {
            match self.bytes[self.pos] {
                b'\\' => {
                    self.pos += 1;
                    if self.pos < self.bytes.len() {
                        if self.bytes[self.pos] == b'\n' {
                            self.line += 1;
                        }
                        self.pos += 1;
                    }
                }
                b'"' => {
                    self.pos += 1;
                    return Ok(());
                }
                b'\n' => {
                    self.line += 1;
                    self.pos += 1;
                }
                _ => self.pos += 1,
            }
        }
        Err(self.err(start_line, "unterminated string literal"))
    }

    fn scan_token(&mut self) -> Result<Tok<'a>, LibertyError> {
        self.skip_ws_and_comments()?;
        let line = self.line;
        if self.pos >= self.bytes.len() {
            return Ok(Tok {
                kind: TokKind::Eof,
                line,
            });
        }
        let b = self.bytes[self.pos];
        let kind = match b {
            b'(' => {
                self.pos += 1;
                TokKind::LParen
            }
            b')' => {
                self.pos += 1;
                TokKind::RParen
            }
            b'{' => {
                self.pos += 1;
                TokKind::LBrace
            }
            b'}' => {
                self.pos += 1;
                TokKind::RBrace
            }
            b':' => {
                self.pos += 1;
                TokKind::Colon
            }
            b';' => {
                self.pos += 1;
                TokKind::Semi
            }
            b',' => {
                self.pos += 1;
                TokKind::Comma
            }
            b'"' => TokKind::Quoted(self.scan_string()?),
            _ => {
                let start = self.pos;
                while self.pos < self.bytes.len() {
                    let c = self.bytes[self.pos];
                    if c.is_ascii_whitespace()
                        || matches!(c, b'(' | b')' | b'{' | b'}' | b':' | b';' | b',' | b'"')
                    {
                        break;
                    }
                    if c == b'/'
                        && self.pos + 1 < self.bytes.len()
                        && matches!(self.bytes[self.pos + 1], b'*' | b'/')
                    {
                        break;
                    }
                    self.pos += 1;
                }
                TokKind::Atom(&self.src[start..self.pos])
            }
        };
        Ok(Tok { kind, line })
    }

    fn scan_string(&mut self) -> Result<String, LibertyError> {
        let start_line = self.line;
        self.pos += 1; // skip opening `"`
        let mut out = String::new();
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            match b {
                b'\\' => {
                    self.pos += 1;
                    if self.pos < self.bytes.len() {
                        let esc = self.bytes[self.pos];
                        if esc == b'\n' {
                            self.line += 1;
                        } else if esc == b'\r'
                            && self.pos + 1 < self.bytes.len()
                            && self.bytes[self.pos + 1] == b'\n'
                        {
                            self.pos += 1;
                            self.line += 1;
                        } else {
                            out.push(esc as char);
                        }
                        self.pos += 1;
                    }
                }
                b'"' => {
                    self.pos += 1;
                    return Ok(out);
                }
                b'\n' => {
                    self.line += 1;
                    out.push('\n');
                    self.pos += 1;
                }
                _ => {
                    let start = self.pos;
                    while self.pos < self.bytes.len()
                        && !matches!(self.bytes[self.pos], b'"' | b'\\' | b'\n')
                    {
                        self.pos += 1;
                    }
                    out.push_str(&self.src[start..self.pos]);
                }
            }
        }
        Err(self.err(start_line, "unterminated string literal"))
    }
}

pub fn tok_desc(k: &TokKind<'_>) -> String {
    match k {
        TokKind::LParen => "`(`".into(),
        TokKind::RParen => "`)`".into(),
        TokKind::LBrace => "`{`".into(),
        TokKind::RBrace => "`}`".into(),
        TokKind::Colon => "`:`".into(),
        TokKind::Semi => "`;`".into(),
        TokKind::Comma => "`,`".into(),
        TokKind::Quoted(s) => format!("\"{s}\""),
        TokKind::Atom(s) => format!("`{s}`"),
        TokKind::Eof => "end of file".into(),
    }
}
