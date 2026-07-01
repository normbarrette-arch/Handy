//! Spoken formatting & punctuation command expansion.
//!
//! Converts spoken command phrases in a transcript into real characters —
//! "dear john comma new paragraph thanks period" becomes:
//!
//! ```text
//! Dear john,
//!
//! Thanks.
//! ```
//!
//! The value is in the spacing/capitalization, not the phrase list: a naive
//! find-replace produces "store . next"; this produces "store. Next".
//!
//! Design:
//! - Only punctuation WE insert is re-spaced. Punctuation the model already
//!   produced (e.g. "3.5", "Dr. Smith") is left untouched — so matched phrases
//!   are first rewritten to private-use sentinel spans, then a render pass
//!   applies spacing based on each span's inferred class.
//! - Phrases match case-insensitively on whole words (`\b…\b`) so "period"
//!   never fires inside "periodic" and "comma" never inside "comment".
//! - Longest phrase first, so "new paragraph" beats "new line" and
//!   "open parenthesis" beats "open paren".

use crate::settings::{AppSettings, SpokenCommand};
use regex::{NoExpand, Regex, RegexBuilder};

// Private-use sentinels bracketing a rewritten command: START, kind digit,
// payload, END. The transcript can't legitimately contain these; we strip
// any that somehow appear before processing.
const MARK_START: char = '\u{E000}';
const MARK_END: char = '\u{E001}';

/// Spacing/attachment class, inferred from the replacement (and phrase, to
/// disambiguate open vs close quotes).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `. ? ! …` — attach to previous word, space after, capitalize next.
    SentenceEnd,
    /// `, ; :` — attach to previous word, space after, no capitalize.
    Clause,
    /// `( [ "`(open) — attach to next word, no gap after.
    Open,
    /// `) ]` and close quote — attach to previous word, space after.
    Close,
    /// `- — /` — no surrounding spaces (glue both sides).
    Tight,
    /// `\n` / `\n\n` — strip surrounding spaces, capitalize next.
    Break,
    /// `& @ $ % # *` and `\t` — literal insert; symbols get single-space
    /// padding, tab glues to the next word.
    Token,
}

impl Kind {
    fn tag(self) -> char {
        match self {
            Kind::SentenceEnd => '0',
            Kind::Clause => '1',
            Kind::Open => '2',
            Kind::Close => '3',
            Kind::Tight => '4',
            Kind::Break => '5',
            Kind::Token => '6',
        }
    }
    fn from_tag(c: char) -> Kind {
        match c {
            '0' => Kind::SentenceEnd,
            '1' => Kind::Clause,
            '2' => Kind::Open,
            '3' => Kind::Close,
            '4' => Kind::Tight,
            '5' => Kind::Break,
            _ => Kind::Token,
        }
    }
}

fn classify(cmd: &SpokenCommand) -> Kind {
    let r = cmd.replacement.as_str();
    if r.contains('\n') {
        return Kind::Break;
    }
    match r {
        "." | "?" | "!" | "…" => Kind::SentenceEnd,
        "," | ";" | ":" => Kind::Clause,
        "-" | "—" | "/" => Kind::Tight,
        "(" | "[" | "{" => Kind::Open,
        ")" | "]" | "}" => Kind::Close,
        "\"" | "'" => {
            let p = cmd.phrase.to_lowercase();
            if p.contains("close") {
                Kind::Close
            } else if p.contains("open") {
                Kind::Open
            } else {
                Kind::Token
            }
        }
        _ => Kind::Token,
    }
}

/// Build a whole-word, case-insensitive regex for a (possibly multi-word)
/// phrase. Internal spaces match one-or-more whitespace for ASR robustness.
fn phrase_regex(phrase: &str) -> Option<Regex> {
    let words: Vec<String> = phrase.split_whitespace().map(regex::escape).collect();
    if words.is_empty() {
        return None;
    }
    let pat = format!(r"\b{}\b", words.join(r"\s+"));
    RegexBuilder::new(&pat).case_insensitive(true).build().ok()
}

/// Expand spoken commands in `text` using the app's settings.
pub fn expand(text: &str, settings: &AppSettings) -> String {
    expand_commands(text, settings.spoken_commands_enabled, &settings.spoken_commands)
}

fn expand_commands(text: &str, enabled: bool, commands: &[SpokenCommand]) -> String {
    if !enabled {
        return text.to_string();
    }

    // Strip any stray sentinels from the input.
    let mut s: String = text
        .chars()
        .filter(|&c| c != MARK_START && c != MARK_END)
        .collect();

    // Longest phrase first (word count, then char length).
    let mut active: Vec<&SpokenCommand> = commands
        .iter()
        .filter(|c| c.enabled && !c.phrase.trim().is_empty() && !c.replacement.is_empty())
        .collect();
    active.sort_by(|a, b| {
        let wa = a.phrase.split_whitespace().count();
        let wb = b.phrase.split_whitespace().count();
        wb.cmp(&wa).then(b.phrase.len().cmp(&a.phrase.len()))
    });

    for cmd in active {
        let Some(re) = phrase_regex(&cmd.phrase) else {
            continue;
        };
        let marker = format!(
            "{}{}{}{}",
            MARK_START,
            classify(cmd).tag(),
            cmd.replacement,
            MARK_END
        );
        // NoExpand: replacement may contain `$` (dollar-sign command).
        s = re.replace_all(&s, NoExpand(marker.as_str())).into_owned();
    }

    render(&s)
}

enum Seg<'a> {
    Text(&'a str),
    Cmd(Kind, &'a str),
}

fn render(marked: &str) -> String {
    // Parse into ordered text / command segments.
    let mut segs: Vec<Seg> = Vec::new();
    let mut rest = marked;
    while let Some(start) = rest.find(MARK_START) {
        if start > 0 {
            segs.push(Seg::Text(&rest[..start]));
        }
        let after = &rest[start + MARK_START.len_utf8()..];
        let Some(kind_ch) = after.chars().next() else {
            break;
        };
        let after_kind = &after[kind_ch.len_utf8()..];
        if let Some(end_rel) = after_kind.find(MARK_END) {
            segs.push(Seg::Cmd(Kind::from_tag(kind_ch), &after_kind[..end_rel]));
            rest = &after_kind[end_rel + MARK_END.len_utf8()..];
        } else {
            segs.push(Seg::Text(after_kind));
            rest = "";
            break;
        }
    }
    if !rest.is_empty() {
        segs.push(Seg::Text(rest));
    }

    // Spacing/capitalization state machine.
    let mut out = String::new();
    let mut cap_next = true; // capitalize the first alphabetic char of output
    let mut glue = false; // suppress the separator space before the next piece

    for seg in segs {
        match seg {
            Seg::Text(raw) => {
                let normalized = raw.split_whitespace().collect::<Vec<_>>().join(" ");
                if normalized.is_empty() {
                    continue;
                }
                if !out.is_empty() && !glue && !out.ends_with('\n') {
                    out.push(' ');
                }
                glue = false;
                if cap_next && normalized.chars().any(|c| c.is_alphabetic()) {
                    out.push_str(&capitalize_first(&normalized));
                    cap_next = false;
                } else {
                    out.push_str(&normalized);
                }
            }
            Seg::Cmd(kind, payload) => match kind {
                Kind::SentenceEnd => {
                    trim_trailing_spaces(&mut out);
                    out.push_str(payload);
                    cap_next = true;
                    glue = false;
                }
                Kind::Clause | Kind::Close => {
                    trim_trailing_spaces(&mut out);
                    out.push_str(payload);
                    glue = false;
                }
                Kind::Open => {
                    if !out.is_empty() && !glue && !ends_with_space_or_newline(&out) {
                        out.push(' ');
                    }
                    out.push_str(payload);
                    glue = true;
                }
                Kind::Tight => {
                    trim_trailing_spaces(&mut out);
                    out.push_str(payload);
                    glue = true;
                }
                Kind::Break => {
                    trim_trailing_spaces(&mut out);
                    out.push_str(payload);
                    cap_next = true;
                    glue = false;
                }
                Kind::Token => {
                    if payload == "\t" {
                        trim_trailing_spaces(&mut out);
                        out.push('\t');
                        glue = true;
                    } else {
                        if !out.is_empty() && !glue && !ends_with_space_or_newline(&out) {
                            out.push(' ');
                        }
                        out.push_str(payload);
                        glue = false;
                    }
                }
            },
        }
    }

    out.trim_start()
        .trim_end_matches(|c: char| c == ' ' || c == '\t')
        .to_string()
}

fn ends_with_space_or_newline(s: &str) -> bool {
    matches!(s.chars().last(), Some(' ') | Some('\t') | Some('\n'))
}

fn trim_trailing_spaces(out: &mut String) {
    while out.ends_with(' ') || out.ends_with('\t') {
        out.pop();
    }
}

fn capitalize_first(s: &str) -> String {
    let mut done = false;
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if !done && c.is_alphabetic() {
            out.extend(c.to_uppercase());
            done = true;
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::default_spoken_commands;

    fn expand_default(text: &str) -> String {
        expand_commands(text, true, &default_spoken_commands())
    }

    #[test]
    fn formats_an_email() {
        let input = "dear John comma new paragraph thanks for your email period \
                     new line best regards comma new line Norm";
        assert_eq!(
            expand_default(input),
            "Dear John,\n\nThanks for your email.\nBest regards,\nNorm"
        );
    }

    #[test]
    fn period_attaches_and_capitalizes() {
        assert_eq!(expand_default("hello period world"), "Hello. World");
    }

    #[test]
    fn comma_attaches_without_capitalizing() {
        assert_eq!(expand_default("apples comma oranges"), "Apples, oranges");
    }

    #[test]
    fn question_mark_capitalizes_next() {
        assert_eq!(expand_default("really question mark yes"), "Really? Yes");
    }

    #[test]
    fn word_boundary_does_not_match_inside_words() {
        // "period" must not fire inside "periodic"; "comma" not inside "comment".
        assert_eq!(expand_default("the periodic comment"), "The periodic comment");
    }

    #[test]
    fn longest_phrase_wins() {
        // "new paragraph" must not be treated as "new" + "paragraph"/"new line".
        assert_eq!(expand_default("a new paragraph b"), "A\n\nB");
        assert_eq!(expand_default("a new line b"), "A\nB");
    }

    #[test]
    fn parentheses_wrap_tightly() {
        assert_eq!(
            expand_default("the value open paren approx close paren works"),
            "The value (approx) works"
        );
    }

    #[test]
    fn hyphen_glues_both_sides() {
        assert_eq!(
            expand_default("state hyphen of hyphen the hyphen art"),
            "State-of-the-art"
        );
    }

    #[test]
    fn disabled_feature_is_passthrough() {
        assert_eq!(
            expand_commands("hello period", false, &default_spoken_commands()),
            "hello period"
        );
    }

    #[test]
    fn disabled_command_is_skipped() {
        let mut cmds = default_spoken_commands();
        for c in cmds.iter_mut() {
            if c.phrase == "period" {
                c.enabled = false;
            }
        }
        // "period" no longer expands; "full stop" still would, but isn't present.
        assert_eq!(expand_commands("hello period there", true, &cmds), "Hello period there");
    }

    #[test]
    fn does_not_touch_model_punctuation() {
        // No command words here — existing decimals/abbreviations survive.
        assert_eq!(expand_default("it was 3.5 on Dr. Smith"), "It was 3.5 on Dr. Smith");
    }

    #[test]
    fn dollar_sign_replacement_is_literal() {
        // NoExpand guard: "$" must not be treated as a regex capture ref.
        assert_eq!(expand_default("pay dollar sign now"), "Pay $ now");
    }
}
