//! Matching text inside a session.
//!
//! Deterministic and local: literal terms, case-insensitive, no regular
//! expressions, no query language, no model. What a query means today is what
//! it will mean next year, and it can be explained in one sentence.
//!
//! # What a query is
//!
//! Whitespace separates terms, and quotes group words into one term:
//!
//! ```text
//! payment orchestrator      two terms - both must appear
//! "payment orchestrator"    one term  - those words, in that order
//! ```
//!
//! Terms are combined with **and**, and they must appear *near each other* —
//! within [`PROXIMITY`] characters.
//!
//! The nearness requirement is not decoration. A single event is not
//! necessarily a sentence: one `tool_result` can hold an entire 50 KB file, so
//! "both words somewhere in the same event" matches a word on line 3 against a
//! word on line 900. Searching eight real sessions for `refresh token` that way
//! returned 705 hits, almost all of them a `refreshSomething` in one place and
//! an unrelated `token` in another. Requiring the terms to be close turns that
//! into the handful of places where somebody was actually discussing refresh
//! tokens.
//!
//! A quoted phrase is a single term and is unaffected — `"refresh token"`
//! matches those words, in that order, and nothing else.
//!
//! # Why there is no regex or SQL here
//!
//! #39 asks for the metacharacter cases to be deliberate. They are: a query is
//! never compiled, never interpolated, and never reaches a query planner. `%`,
//! `_`, `*`, `'`, `;`, `--`, `.*` and `(` are ordinary characters to match
//! literally, so the injection surface those tests probe does not exist rather
//! than being escaped away. That is the reason to prefer literal matching here
//! even though it gives up some expressiveness.

use std::fmt;

/// How close separate terms have to be to count as a match, in characters.
///
/// Roughly a paragraph. Chosen against real transcripts: `refresh token` over
/// eight real sessions returned 705 hits without a proximity limit and 20 with
/// this one, and the 20 are the places somebody was actually talking about
/// refresh tokens.
pub const PROXIMITY: usize = 400;

/// Why a query could not be used.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum QueryError {
    /// Nothing to search for.
    ///
    /// An empty query matches everything, which is never what someone meant to
    /// ask for, and is indistinguishable from a shell quoting mistake.
    #[error("the search query is empty")]
    Empty,
}

/// A parsed search query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    terms: Vec<String>,
}

impl Query {
    /// Parse what the user typed.
    pub fn parse(input: &str) -> Result<Self, QueryError> {
        let terms = tokenize(input);
        if terms.is_empty() {
            return Err(QueryError::Empty);
        }
        Ok(Self { terms })
    }

    /// The terms, as they will be matched.
    pub fn terms(&self) -> &[String] {
        &self.terms
    }

    /// Whether the query matches somewhere in `haystack`.
    pub fn matches(&self, haystack: &str) -> bool {
        self.first_match(haystack).is_some()
    }

    /// Where the first place all the terms appear together begins.
    ///
    /// "Together" means inside a window of [`PROXIMITY`] characters. A
    /// single-term query is just the first occurrence of that term.
    ///
    /// The offset returned is the start of the window — the earliest term in
    /// it — so a snippet taken from here shows the whole matching region rather
    /// than starting at whichever term the user happened to type first.
    pub fn first_match(&self, haystack: &str) -> Option<usize> {
        // Every place each term occurs. Bailing out early if any term is
        // missing entirely saves scanning for the rest.
        let mut per_term: Vec<Vec<usize>> = Vec::with_capacity(self.terms.len());
        for term in &self.terms {
            let occurrences = occurrences_ignoring_case(haystack, term);
            if occurrences.is_empty() {
                return None;
            }
            per_term.push(occurrences);
        }

        if per_term.len() == 1 {
            return per_term[0].first().copied();
        }

        // Anchor on each occurrence of the rarest term rather than the first
        // one typed. On a large tool result a common word like "token" can
        // occur hundreds of times while the other term occurs twice, and
        // anchoring on the rare one keeps this cheap.
        let anchor = (0..per_term.len())
            .min_by_key(|&i| per_term[i].len())
            .expect("at least one term");

        let mut best: Option<usize> = None;
        for &at in &per_term[anchor] {
            let window_start = at.saturating_sub(PROXIMITY);
            let window_end = at + PROXIMITY;

            // Every other term has to have an occurrence inside the window.
            let mut earliest = at;
            let all_present = per_term.iter().enumerate().all(|(i, occurrences)| {
                if i == anchor {
                    return true;
                }
                match occurrences
                    .iter()
                    .copied()
                    .find(|&o| o >= window_start && o <= window_end)
                {
                    Some(o) => {
                        earliest = earliest.min(o);
                        true
                    }
                    None => false,
                }
            });

            if all_present {
                best = Some(best.map_or(earliest, |b: usize| b.min(earliest)));
            }
        }
        best
    }
}

impl fmt::Display for Query {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let shown: Vec<String> = self
            .terms
            .iter()
            .map(|t| {
                if t.contains(char::is_whitespace) {
                    format!("{t:?}")
                } else {
                    t.clone()
                }
            })
            .collect();
        f.write_str(&shown.join(" "))
    }
}

/// Split a query into terms, honouring quotes.
///
/// An unclosed quote takes the rest of the line. That is a typo, and guessing
/// the user meant a phrase is friendlier than refusing to search.
fn tokenize(input: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut current = String::new();
    let mut quoted = false;

    for c in input.chars() {
        match c {
            '"' => {
                if quoted {
                    // A closing quote ends the term even if it is empty, so
                    // `""` is not silently dropped into nothing.
                    terms.push(std::mem::take(&mut current));
                    quoted = false;
                } else {
                    if !current.is_empty() {
                        terms.push(std::mem::take(&mut current));
                    }
                    quoted = true;
                }
            }
            c if c.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    terms.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        terms.push(current);
    }
    terms.retain(|t| !t.is_empty());
    terms
}

/// Find `needle` in `haystack`, ignoring case, returning a byte offset into
/// `haystack`.
///
/// The offset is into the original text, not a lowercased copy. Case folding
/// can change a string's length — `İ` lowercases to two chars — so an offset
/// taken from a folded copy can land in the wrong place, or inside a character.
pub fn find_ignoring_case(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }

    // The overwhelmingly common case, and the fast one. An ASCII byte can never
    // occur inside a multi-byte UTF-8 sequence, so a byte-wise match of an
    // ASCII needle always begins on a character boundary.
    if needle.is_ascii() && haystack.is_ascii() {
        let (h, n) = (haystack.as_bytes(), needle.as_bytes());
        if n.len() > h.len() {
            return None;
        }
        return (0..=h.len() - n.len()).find(|&i| h[i..i + n.len()].eq_ignore_ascii_case(n));
    }

    // Anything else: compare character by character, folding as we go.
    let needle_lower: Vec<char> = needle.chars().flat_map(char::to_lowercase).collect();
    for (offset, _) in haystack.char_indices() {
        let mut candidate = haystack[offset..].chars().flat_map(char::to_lowercase);
        if needle_lower.iter().all(|&c| candidate.next() == Some(c)) {
            return Some(offset);
        }
    }
    None
}

/// Every place `needle` occurs in `haystack`, ignoring case.
///
/// Overlapping occurrences are included: the next search resumes one character
/// after the previous start, not after the whole match, so `aa` is found twice
/// in `aaa`. Missing one would let a proximity window fail to close.
fn occurrences_ignoring_case(haystack: &str, needle: &str) -> Vec<usize> {
    let mut found = Vec::new();
    let mut from = 0;
    while from < haystack.len() {
        match find_ignoring_case(&haystack[from..], needle) {
            Some(at) => {
                let absolute = from + at;
                found.push(absolute);
                // Advance one character, not one byte, so the next slice still
                // starts on a boundary.
                from = absolute
                    + haystack[absolute..]
                        .chars()
                        .next()
                        .map_or(1, char::len_utf8);
            }
            None => break,
        }
    }
    found
}

/// A readable fragment of `text` around the byte offset `at`.
///
/// Whitespace is collapsed, because a transcript is full of newlines and
/// indentation and a snippet that reproduces them is unreadable in a listing.
/// Nothing is highlighted here — that is the caller's business.
pub fn snippet(text: &str, at: usize, width: usize) -> String {
    let start = floor_boundary(text, at.saturating_sub(width / 2));
    let end = ceil_boundary(text, (at + width / 2).min(text.len()));

    let mut out = String::with_capacity(end - start + 2);
    if start > 0 {
        out.push('…');
    }

    let mut last_was_space = false;
    for c in text[start..end].chars() {
        if c.is_whitespace() || c.is_control() {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
        } else {
            out.push(c);
            last_was_space = false;
        }
    }
    let trimmed = out.trim().to_string();

    let mut out = trimmed;
    if end < text.len() {
        out.push('…');
    }
    out
}

/// The nearest character boundary at or below `index`.
fn floor_boundary(text: &str, mut index: usize) -> usize {
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// The nearest character boundary at or above `index`.
fn ceil_boundary(text: &str, mut index: usize) -> usize {
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(s: &str) -> Query {
        Query::parse(s).expect("valid query")
    }

    #[test]
    fn a_single_word_matches_anywhere_in_the_text() {
        let q = query("orchestrator");
        assert!(q.matches("the payment orchestrator retries"));
        assert!(q.matches("orchestrator"));
        assert!(!q.matches("the payment service retries"));
    }

    #[test]
    fn matching_ignores_case() {
        let q = query("Payment");
        assert!(q.matches("PAYMENT"));
        assert!(q.matches("payment"));
        assert!(q.matches("PaYmEnT"));
    }

    #[test]
    fn separate_words_must_all_appear() {
        // "and", not "or". Either would be defensible; this one is stated in
        // the module docs and is what the tests pin.
        let q = query("payment orchestrator");
        assert!(q.matches("the orchestrator handles payment"));
        assert!(!q.matches("the orchestrator handles refunds"));
        assert!(!q.matches("payment was taken"));
    }

    #[test]
    fn terms_far_apart_are_not_a_match() {
        // The rule that makes search usable on real transcripts. A tool result
        // can be an entire file, and two words a thousand characters apart are
        // not what anyone meant by searching for both.
        let q = query("payment orchestrator");
        let far = format!("payment{}orchestrator", " ".repeat(PROXIMITY * 2));
        assert!(!q.matches(&far));

        let near = format!("payment{}orchestrator", " ".repeat(10));
        assert!(q.matches(&near));
    }

    #[test]
    fn a_match_is_found_wherever_in_the_text_the_terms_come_together() {
        // The terms appear apart early on and together later. Scanning must not
        // stop at the first occurrence of either.
        let q = query("payment orchestrator");
        let text = format!(
            "payment{}unrelated{}the payment orchestrator retries",
            " ".repeat(PROXIMITY * 2),
            " ".repeat(PROXIMITY * 2)
        );
        let at = q.first_match(&text).expect("should match at the end");
        assert!(text[at..].starts_with("payment orchestrator"), "{at}");
    }

    #[test]
    fn a_single_term_is_unaffected_by_proximity() {
        let q = query("orchestrator");
        let text = format!("{}orchestrator", "x".repeat(PROXIMITY * 5));
        assert!(q.matches(&text));
    }

    #[test]
    fn three_terms_must_all_be_close() {
        let q = query("retry backoff jitter");
        assert!(q.matches("retry with exponential backoff and jitter"));
        // Two together, the third far away.
        let split = format!("retry backoff{}jitter", " ".repeat(PROXIMITY * 2));
        assert!(!q.matches(&split));
    }

    #[test]
    fn overlapping_occurrences_are_all_considered() {
        // If only the first occurrence of each term were used, this would fail:
        // the first "aa" is far from "target", the second is next to it.
        let q = query("aa target");
        let text = format!("aa{}aaa target", " ".repeat(PROXIMITY * 2));
        assert!(q.matches(&text));
    }

    #[test]
    fn a_quoted_phrase_is_one_term() {
        let q = query("\"payment orchestrator\"");
        assert_eq!(q.terms(), ["payment orchestrator"]);
        assert!(q.matches("the payment orchestrator retries"));
        // The words are present but not adjacent, so the phrase is not.
        assert!(!q.matches("the orchestrator handles payment"));
    }

    #[test]
    fn quoted_and_bare_terms_mix() {
        let q = query("\"tool result\" failed");
        assert_eq!(q.terms(), ["tool result", "failed"]);
        assert!(q.matches("the tool result failed"));
        assert!(!q.matches("the tool result succeeded"));
    }

    #[test]
    fn an_unclosed_quote_takes_the_rest() {
        // A typo. Refusing to search would be less useful than guessing.
        let q = query("\"payment orchestrator");
        assert_eq!(q.terms(), ["payment orchestrator"]);
    }

    #[test]
    fn an_empty_query_is_refused() {
        // It would match every event of every session, which nobody means.
        assert_eq!(Query::parse(""), Err(QueryError::Empty));
        assert_eq!(Query::parse("   "), Err(QueryError::Empty));
        assert_eq!(Query::parse("\t\n"), Err(QueryError::Empty));
        assert_eq!(Query::parse("\"\""), Err(QueryError::Empty));
    }

    #[test]
    fn metacharacters_are_ordinary_text() {
        // The heart of #39. None of these mean anything to Recall: there is no
        // pattern to compile and no statement to interpolate into, so they are
        // matched exactly as typed.
        for raw in [
            "100%",
            "_private",
            "a*b",
            "it's",
            "; DROP TABLE sessions; --",
            ".*",
            "(a|b)",
            "back\\slash",
            "[bracket]",
            "null\0byte",
        ] {
            let q = Query::parse(raw).expect("parses");
            let haystack = format!("before {raw} after");
            assert!(q.matches(&haystack), "{raw:?} did not match itself");
        }
    }

    #[test]
    fn a_wildcard_does_not_act_as_a_wildcard() {
        // If `*` were ever treated as a pattern this would start passing, and
        // the promise that queries are literal would be quietly broken.
        let q = query("pay*ment");
        assert!(!q.matches("payment"));
        assert!(q.matches("pay*ment"));
    }

    #[test]
    fn sql_wildcards_are_not_wildcards_either() {
        let q = query("pay%");
        assert!(!q.matches("payment"));
        assert!(q.matches("pay% is literal"));
    }

    #[test]
    fn the_offset_reported_is_the_earliest_of_the_terms() {
        let q = query("orchestrator payment");
        let text = "the payment orchestrator";
        // "payment" is at 4 and comes first, even though it was typed second.
        assert_eq!(q.first_match(text), Some(4));
    }

    #[test]
    fn no_offset_when_the_query_does_not_match() {
        assert_eq!(query("payment refunds").first_match("only payment"), None);
    }

    #[test]
    fn matching_works_on_text_that_is_not_ascii() {
        let q = query("Café");
        assert!(q.matches("le CAFÉ est ouvert"));
        assert!(!q.matches("le the est ouvert"));
    }

    #[test]
    fn an_offset_in_non_ascii_text_lands_on_a_character_boundary() {
        // The reason offsets are taken from the original string rather than a
        // lowercased copy: slicing at a byte that is mid-character panics.
        let text = "héllo wörld payment";
        let at = query("payment").first_match(text).expect("matches");
        assert!(text.is_char_boundary(at));
        assert!(text[at..].starts_with("payment"));
    }

    #[test]
    fn a_snippet_collapses_the_whitespace_a_transcript_is_full_of() {
        let text = "some text\n\n    with   ragged\n\tindentation here";
        let s = snippet(text, 0, 200);
        assert!(!s.contains('\n'), "{s:?}");
        assert!(!s.contains("  "), "{s:?}");
        assert!(s.contains("with ragged indentation"), "{s:?}");
    }

    #[test]
    fn a_snippet_marks_where_it_was_cut() {
        let text = "a".repeat(500);
        let s = snippet(&text, 250, 40);
        assert!(s.starts_with('…'), "{s:?}");
        assert!(s.ends_with('…'), "{s:?}");
        assert!(s.chars().count() <= 60, "{} chars", s.chars().count());
    }

    #[test]
    fn a_short_text_is_not_marked_as_cut() {
        assert_eq!(snippet("all of it", 0, 200), "all of it");
    }

    #[test]
    fn a_snippet_never_splits_a_character() {
        // Every offset in a multi-byte string, including ones mid-character.
        let text = "héllo wörld — a session about café and naïve encodings";
        for at in 0..text.len() {
            let s = snippet(text, at, 20);
            assert!(!s.is_empty() || text.is_empty());
        }
    }

    #[test]
    fn a_query_prints_the_way_it_was_meant() {
        assert_eq!(
            query("payment orchestrator").to_string(),
            "payment orchestrator"
        );
        assert_eq!(
            query("\"payment orchestrator\"").to_string(),
            "\"payment orchestrator\""
        );
    }
}
