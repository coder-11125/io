//! A small Vim-style editing mode for the prompt (`/vim` toggles it).
//!
//! Insert mode is the normal prompt. Esc switches to Normal mode, where
//! single keys edit the line: motions (`h l 0 ^ $ w b e j k`), edits
//! (`x X D C s S r ~`), operators with motions (`d c y` + `w b e $ 0 ^ h l`,
//! doubled for the whole line), put (`p P`), undo (`u`), and the ways back
//! into Insert (`i a I A o O`). Enter submits in either mode.
//!
//! The logic is pure — it edits a `String` and a byte cursor — so it is
//! tested without a terminal.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Insert,
    Normal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Register {
    text: String,
    linewise: bool,
}

#[derive(Debug)]
pub struct Vim {
    pub mode: Mode,
    /// A key waiting for its second half: an operator (`d c y`) or `r`.
    pending: Option<char>,
    register: Option<Register>,
    undo: Option<(String, usize)>,
}

impl Default for Vim {
    fn default() -> Self {
        Self {
            mode: Mode::Insert,
            pending: None,
            register: None,
            undo: None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Space,
    Word,
    Punct,
}

fn class(c: char) -> Class {
    if c.is_whitespace() {
        Class::Space
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

fn line_start(chars: &[char], i: usize) -> usize {
    chars[..i.min(chars.len())]
        .iter()
        .rposition(|&c| c == '\n')
        .map(|p| p + 1)
        .unwrap_or(0)
}

fn line_end(chars: &[char], i: usize) -> usize {
    chars[i.min(chars.len())..]
        .iter()
        .position(|&c| c == '\n')
        .map(|p| i + p)
        .unwrap_or(chars.len())
}

fn to_chars(buf: &str, byte_cursor: usize) -> (Vec<char>, usize) {
    let cursor = byte_cursor.min(buf.len());
    let mut cursor = cursor;
    while !buf.is_char_boundary(cursor) {
        cursor -= 1;
    }
    (buf.chars().collect(), buf[..cursor].chars().count())
}

fn byte_index(chars: &[char], ci: usize) -> usize {
    chars[..ci.min(chars.len())]
        .iter()
        .map(|c| c.len_utf8())
        .sum()
}

/// Start of the next word after `i` (may cross lines).
fn next_word(chars: &[char], i: usize) -> usize {
    let n = chars.len();
    let mut i = i;
    if i >= n {
        return n;
    }
    let start = class(chars[i]);
    if start != Class::Space {
        while i < n && class(chars[i]) == start {
            i += 1;
        }
    }
    while i < n && class(chars[i]) == Class::Space && chars[i] != '\n' {
        i += 1;
    }
    // A newline counts as the start of the next word's line.
    if i < n && chars[i] == '\n' {
        i += 1;
        while i < n && class(chars[i]) == Class::Space && chars[i] != '\n' {
            i += 1;
        }
    }
    i
}

fn prev_word(chars: &[char], i: usize) -> usize {
    let mut i = i.min(chars.len());
    while i > 0 && class(chars[i - 1]) == Class::Space {
        i -= 1;
    }
    if i == 0 {
        return 0;
    }
    let c = class(chars[i - 1]);
    while i > 0 && class(chars[i - 1]) == c {
        i -= 1;
    }
    i
}

/// Last character of the current or next word.
fn word_end(chars: &[char], i: usize) -> usize {
    let n = chars.len();
    if n == 0 {
        return 0;
    }
    let mut i = (i + 1).min(n - 1);
    while i < n - 1 && class(chars[i]) == Class::Space {
        i += 1;
    }
    let c = class(chars[i]);
    while i + 1 < n && class(chars[i + 1]) == c && c != Class::Space {
        i += 1;
    }
    i
}

/// Where motion `m` from `i` lands and whether the target is included.
fn motion(chars: &[char], i: usize, m: char) -> Option<(usize, bool)> {
    let (ls, le) = (line_start(chars, i), line_end(chars, i));
    match m {
        'h' => (i > ls).then(|| (i - 1, false)),
        'l' => (i < le).then(|| (i + 1, false)),
        '0' => Some((ls, false)),
        '^' => {
            let mut p = ls;
            while p < le && chars[p].is_whitespace() {
                p += 1;
            }
            Some((p, false))
        }
        '$' => Some((le.saturating_sub(1).max(ls), le > ls)),
        'w' => Some((next_word(chars, i), false)),
        'b' => Some((prev_word(chars, i), false)),
        'e' => Some((word_end(chars, i), true)),
        _ => None,
    }
}

impl Vim {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn label(&self) -> &'static str {
        match self.mode {
            Mode::Insert => "-- INSERT --",
            Mode::Normal => "-- NORMAL --",
        }
    }

    /// Esc in Insert mode: switch to Normal, stepping the cursor back onto
    /// the last typed character like Vim does.
    pub fn enter_normal(&mut self, buf: &str, cursor: &mut usize) {
        self.mode = Mode::Normal;
        self.pending = None;
        let (chars, ci) = to_chars(buf, *cursor);
        let ls = line_start(&chars, ci);
        let ci = if ci > ls { ci - 1 } else { ci };
        *cursor = byte_index(&chars, ci);
    }

    fn remember(&mut self, buf: &str, cursor: usize) {
        self.undo = Some((buf.to_string(), cursor));
    }

    /// Handle one key in Normal mode. Returns whether the buffer or cursor
    /// may have changed (so the caller redraws).
    pub fn handle_normal(&mut self, key: char, buf: &mut String, cursor: &mut usize) -> bool {
        let (mut chars, mut ci) = to_chars(buf, *cursor);
        let pending = self.pending.take();

        // Second key of `r<char>`.
        if pending == Some('r') {
            if ci < chars.len() && chars[ci] != '\n' {
                self.remember(buf, *cursor);
                chars[ci] = key;
                self.store(&chars, ci, buf, cursor);
            }
            return true;
        }
        // Operator + motion (or doubled operator = whole line).
        if let Some(op) = pending {
            self.operator(op, key, &mut chars, &mut ci, buf, cursor);
            return true;
        }

        match key {
            'h' | 'l' | '0' | '^' | 'w' | 'b' | 'e' => {
                if let Some((t, _)) = motion(&chars, ci, key) {
                    // Normal mode keeps the cursor on a character.
                    let le = line_end(&chars, t.min(chars.len()));
                    let ls = line_start(&chars, t.min(chars.len()));
                    ci = if t >= le && le > ls {
                        le - 1
                    } else {
                        t.min(chars.len())
                    };
                    if ci == chars.len() && ci > 0 {
                        ci -= 1;
                    }
                }
            }
            '$' => ci = motion(&chars, ci, '$').map(|(t, _)| t).unwrap_or(ci),
            'j' | 'k' => {
                let col = ci - line_start(&chars, ci);
                let target_line = if key == 'j' {
                    let le = line_end(&chars, ci);
                    (le < chars.len()).then_some(le + 1)
                } else {
                    let ls = line_start(&chars, ci);
                    (ls > 0).then(|| line_start(&chars, ls - 1))
                };
                if let Some(ts) = target_line {
                    let te = line_end(&chars, ts);
                    ci = (ts + col).min(te.saturating_sub(1).max(ts));
                }
            }
            'x' => {
                if ci < chars.len() && chars[ci] != '\n' {
                    self.remember(buf, *cursor);
                    self.register = Some(Register {
                        text: chars[ci].to_string(),
                        linewise: false,
                    });
                    chars.remove(ci);
                }
            }
            'X' => {
                if ci > line_start(&chars, ci) {
                    self.remember(buf, *cursor);
                    self.register = Some(Register {
                        text: chars[ci - 1].to_string(),
                        linewise: false,
                    });
                    chars.remove(ci - 1);
                    ci -= 1;
                }
            }
            'D' | 'C' => {
                let le = line_end(&chars, ci);
                self.remember(buf, *cursor);
                self.register = Some(Register {
                    text: chars[ci..le].iter().collect(),
                    linewise: false,
                });
                chars.drain(ci..le);
                if key == 'C' {
                    self.mode = Mode::Insert;
                }
            }
            's' => {
                if ci < chars.len() && chars[ci] != '\n' {
                    self.remember(buf, *cursor);
                    self.register = Some(Register {
                        text: chars[ci].to_string(),
                        linewise: false,
                    });
                    chars.remove(ci);
                }
                self.mode = Mode::Insert;
            }
            'S' => {
                self.remember(buf, *cursor);
                let (ls, le) = (line_start(&chars, ci), line_end(&chars, ci));
                self.register = Some(Register {
                    text: chars[ls..le].iter().collect(),
                    linewise: true,
                });
                chars.drain(ls..le);
                ci = ls;
                self.mode = Mode::Insert;
            }
            '~' => {
                if ci < chars.len() && chars[ci] != '\n' {
                    self.remember(buf, *cursor);
                    let c = chars[ci];
                    chars[ci] = if c.is_uppercase() {
                        c.to_lowercase().next().unwrap_or(c)
                    } else {
                        c.to_uppercase().next().unwrap_or(c)
                    };
                    if ci + 1 < line_end(&chars, ci) {
                        ci += 1;
                    }
                }
            }
            'r' | 'd' | 'c' | 'y' => self.pending = Some(key),
            'p' | 'P' => {
                if let Some(reg) = self.register.clone() {
                    self.remember(buf, *cursor);
                    if reg.linewise {
                        let at = if key == 'p' {
                            let le = line_end(&chars, ci);
                            if le < chars.len() {
                                le + 1
                            } else {
                                chars.push('\n');
                                chars.len()
                            }
                        } else {
                            line_start(&chars, ci)
                        };
                        let mut text: Vec<char> = reg.text.chars().collect();
                        text.push('\n');
                        // Pasting at the very end leaves no trailing newline.
                        if at == chars.len() {
                            text.pop();
                        }
                        chars.splice(at..at, text);
                        ci = at;
                    } else {
                        let at = if key == 'p' && ci < line_end(&chars, ci) {
                            ci + 1
                        } else {
                            ci
                        };
                        let text: Vec<char> = reg.text.chars().collect();
                        let len = text.len();
                        chars.splice(at..at, text);
                        ci = at + len.saturating_sub(1);
                    }
                }
            }
            'u' => {
                if let Some((old, old_cursor)) = self.undo.take() {
                    self.undo = Some((buf.clone(), *cursor));
                    *buf = old;
                    *cursor = old_cursor.min(buf.len());
                    return true;
                }
                return false;
            }
            'i' => self.mode = Mode::Insert,
            'a' => {
                if ci < line_end(&chars, ci) {
                    ci += 1;
                }
                self.mode = Mode::Insert;
            }
            'I' => {
                ci = motion(&chars, ci, '^').map(|(t, _)| t).unwrap_or(ci);
                self.mode = Mode::Insert;
            }
            'A' => {
                ci = line_end(&chars, ci);
                self.mode = Mode::Insert;
            }
            'o' | 'O' => {
                self.remember(buf, *cursor);
                let at = if key == 'o' {
                    line_end(&chars, ci)
                } else {
                    line_start(&chars, ci)
                };
                chars.insert(at, '\n');
                ci = if key == 'o' { at + 1 } else { at };
                self.mode = Mode::Insert;
            }
            _ => return false,
        }
        self.store(&chars, ci, buf, cursor);
        true
    }

    fn store(&mut self, chars: &[char], ci: usize, buf: &mut String, cursor: &mut usize) {
        *buf = chars.iter().collect();
        let mut ci = ci.min(chars.len());
        // Normal mode never rests past the end of a line.
        if self.mode == Mode::Normal
            && ci > 0
            && ci == line_end(chars, ci)
            && ci > line_start(chars, ci)
        {
            ci -= 1;
        }
        *cursor = byte_index(chars, ci);
    }

    fn operator(
        &mut self,
        op: char,
        key: char,
        chars: &mut Vec<char>,
        ci: &mut usize,
        buf: &mut String,
        cursor: &mut usize,
    ) {
        let before = (buf.clone(), *cursor);
        // Doubled operator: the whole line.
        if key == op {
            let (ls, le) = (line_start(chars, *ci), line_end(chars, *ci));
            let text: String = chars[ls..le].iter().collect();
            self.register = Some(Register {
                text,
                linewise: true,
            });
            if op == 'y' {
                return;
            }
            self.undo = Some(before);
            if op == 'c' {
                chars.drain(ls..le);
                *ci = ls;
                self.mode = Mode::Insert;
            } else if le < chars.len() {
                chars.drain(ls..=le);
                *ci = ls;
            } else if ls > 0 {
                chars.drain(ls - 1..le);
                *ci = line_start(chars, ls - 1);
            } else {
                chars.clear();
                *ci = 0;
            }
            self.store(chars, *ci, buf, cursor);
            return;
        }
        let Some((mut target, inclusive)) = motion(chars, *ci, key) else {
            return;
        };
        // `cw` changes to the end of the word (not the space after it), and
        // `dw` on a line's last word stops at the end of the line.
        if key == 'w' && op == 'c' && *ci < chars.len() && class(chars[*ci]) != Class::Space {
            let kind = class(chars[*ci]);
            let mut j = *ci;
            while j + 1 < chars.len() && class(chars[j + 1]) == kind {
                j += 1;
            }
            target = j + 1;
        } else if key == 'w' {
            target = target.min(line_end(chars, *ci));
        }
        let (mut start, mut end) = if target >= *ci {
            (*ci, target)
        } else {
            (target, *ci)
        };
        if inclusive && target >= *ci && key != 'w' {
            end = (end + 1).min(chars.len());
        }
        if key == '$' {
            end = line_end(chars, *ci);
        }
        end = end.min(chars.len());
        start = start.min(end);
        let text: String = chars[start..end].iter().collect();
        self.register = Some(Register {
            text,
            linewise: false,
        });
        if op == 'y' {
            return;
        }
        self.undo = Some(before);
        chars.drain(start..end);
        *ci = start;
        if op == 'c' {
            self.mode = Mode::Insert;
        }
        self.store(chars, *ci, buf, cursor);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run `keys` in Normal mode over `text` with the cursor at `at` (a char
    /// index) and return the buffer, cursor char index and mode.
    fn run(text: &str, at: usize, keys: &str) -> (String, usize, Mode) {
        let mut vim = Vim::new();
        vim.mode = Mode::Normal;
        let mut buf = text.to_string();
        let mut cursor = byte_index(&buf.chars().collect::<Vec<_>>(), at);
        for k in keys.chars() {
            vim.handle_normal(k, &mut buf, &mut cursor);
        }
        (buf.clone(), buf[..cursor].chars().count(), vim.mode)
    }

    fn text(t: &str, at: usize, keys: &str) -> String {
        run(t, at, keys).0
    }

    #[test]
    fn esc_steps_the_cursor_back_onto_a_character() {
        let mut vim = Vim::new();
        let mut cursor = 5;
        vim.enter_normal("hello", &mut cursor);
        assert_eq!((vim.mode, cursor), (Mode::Normal, 4));
        let mut cursor = 0;
        vim.enter_normal("hello", &mut cursor);
        assert_eq!(cursor, 0);
        assert_eq!(vim.label(), "-- NORMAL --");
    }

    #[test]
    fn motions_move_the_cursor() {
        assert_eq!(run("one two three", 0, "w").1, 4);
        assert_eq!(run("one two three", 0, "ww").1, 8);
        assert_eq!(run("one two three", 12, "b").1, 8);
        assert_eq!(run("one two three", 0, "e").1, 2);
        assert_eq!(run("one two three", 5, "0").1, 0);
        assert_eq!(run("one two three", 0, "$").1, 12);
        assert_eq!(run("  indented", 7, "^").1, 2);
        assert_eq!(run("abc", 1, "h").1, 0);
        assert_eq!(run("abc", 0, "h").1, 0);
        assert_eq!(run("abc", 2, "l").1, 2, "stays on the last character");
        assert_eq!(run("foo.bar", 0, "w").1, 3, "punctuation is its own word");
    }

    #[test]
    fn j_and_k_move_between_lines_keeping_the_column() {
        assert_eq!(run("abcd\nxy\nlonger", 3, "j").1, 6);
        assert_eq!(run("abcd\nxy\nlonger", 6, "j").1, 9);
        assert_eq!(run("abcd\nxy\nlonger", 10, "k").1, 6);
        assert_eq!(run("abcd\nxy", 1, "k").1, 1, "no line above");
    }

    #[test]
    fn single_key_edits() {
        assert_eq!(text("hello", 1, "x"), "hllo");
        assert_eq!(text("hello", 1, "X"), "ello");
        assert_eq!(text("hello world", 5, "D"), "hello");
        assert_eq!(text("hello", 0, "~"), "Hello");
        assert_eq!(text("Hello", 0, "~"), "hello");
        assert_eq!(text("cat", 0, "rb"), "bat");
        assert_eq!(text("ab\ncd", 0, "rxjrx"), "xb\nxd");
        let (t, _, mode) = run("hello", 0, "s");
        assert_eq!((t.as_str(), mode), ("ello", Mode::Insert));
        let (t, _, mode) = run("hello world", 5, "C");
        assert_eq!((t.as_str(), mode), ("hello", Mode::Insert));
    }

    #[test]
    fn operators_with_motions() {
        assert_eq!(text("one two three", 0, "dw"), "two three");
        assert_eq!(text("one two three", 4, "dw"), "one three");
        assert_eq!(text("one two three", 8, "dw"), "one two ");
        assert_eq!(text("one two three", 4, "db"), "two three");
        assert_eq!(text("one two three", 4, "d$"), "one ");
        assert_eq!(text("one two three", 4, "d0"), "two three");
        assert_eq!(text("one two three", 0, "de"), " two three");
        assert_eq!(text("hello", 1, "dl"), "hllo");
        let (t, _, mode) = run("one two", 0, "cw");
        assert_eq!((t.as_str(), mode), (" two", Mode::Insert));
        let (t, _, mode) = run("one two", 4, "c$");
        assert_eq!((t.as_str(), mode), ("one ", Mode::Insert));
    }

    #[test]
    fn line_operators() {
        assert_eq!(text("a\nb\nc", 2, "dd"), "a\nc");
        assert_eq!(text("a\nb\nc", 0, "dd"), "b\nc");
        assert_eq!(text("a\nb\nc", 4, "dd"), "a\nb");
        assert_eq!(text("only", 0, "dd"), "");
        let (t, _, mode) = run("a\nbb\nc", 2, "cc");
        assert_eq!((t.as_str(), mode), ("a\n\nc", Mode::Insert));
        assert_eq!(text("abc", 1, "yy"), "abc");
    }

    #[test]
    fn put_after_yank_or_delete() {
        assert_eq!(text("one two", 0, "ywP"), "one one two");
        assert_eq!(text("one two", 0, "dwp"), "tone wo");
        assert_eq!(text("a\nb", 0, "yyp"), "a\na\nb");
        assert_eq!(text("a\nb", 0, "yyP"), "a\na\nb");
        assert_eq!(text("a\nb", 2, "yyp"), "a\nb\nb");
        assert_eq!(text("a\nb\nc", 0, "ddp"), "b\na\nc");
        assert_eq!(text("abc", 0, "xp"), "bac");
        assert_eq!(text("abc", 0, "p"), "abc", "nothing to put");
    }

    #[test]
    fn undo_reverts_the_last_change_and_undo_again_redoes() {
        let mut vim = Vim::new();
        vim.mode = Mode::Normal;
        let mut buf = "hello".to_string();
        let mut cursor = 0;
        vim.handle_normal('x', &mut buf, &mut cursor);
        assert_eq!(buf, "ello");
        vim.handle_normal('u', &mut buf, &mut cursor);
        assert_eq!(buf, "hello");
        vim.handle_normal('u', &mut buf, &mut cursor);
        assert_eq!(buf, "ello");
        let mut fresh = Vim::new();
        let mut b = "x".to_string();
        let mut c = 0;
        assert!(!fresh.handle_normal('u', &mut b, &mut c));
    }

    #[test]
    fn keys_that_enter_insert_mode_place_the_cursor() {
        assert_eq!(run("abc", 1, "i"), ("abc".into(), 1, Mode::Insert));
        assert_eq!(run("abc", 1, "a"), ("abc".into(), 2, Mode::Insert));
        assert_eq!(run("abc", 0, "A"), ("abc".into(), 3, Mode::Insert));
        assert_eq!(run("  abc", 4, "I"), ("  abc".into(), 2, Mode::Insert));
        assert_eq!(run("ab\ncd", 0, "o"), ("ab\n\ncd".into(), 3, Mode::Insert));
        assert_eq!(run("ab\ncd", 3, "O"), ("ab\n\ncd".into(), 3, Mode::Insert));
    }

    #[test]
    fn unknown_keys_and_edge_cases_do_nothing_harmful() {
        assert_eq!(text("", 0, "xXDdw~"), "");
        assert_eq!(text("abc", 0, "z"), "abc");
        assert_eq!(text("abc", 0, "dz"), "abc");
        let (t, _, _) = run("héllo wörld", 0, "wx");
        assert_eq!(t, "héllo örld");
        assert_eq!(text("日本語 テキスト", 0, "dw"), "テキスト");
    }
}
