//! Pre-postprocessing text norms: dictionary prompt bias + snippet
//! expansion. Faithful port of OpenWhispr's `dictionaryPromptCap`
//! budgets and `snippets.ts` matcher (regex-free: equivalent
//! longest-first scan with the same boundary/folding rules).
//!
//! Scope is deliberately pre-LLM: STT bias goes out with the next
//! inference, snippet expansion rewrites the transcript before
//! `post_process`. The LLM stage (when it lands) receives the
//! expanded final, never re-substitutes.
//!
//! Deliberate simplifications vs the reference:
//! - No NFC normalization (needs `unicode-normalization`; edge-case
//!   combining marks may miss). Case/İ folding is manual and exact.
//! - Boundary = start/end or non-(alphanumeric/`_`); the reference
//!   uses Unicode P/S categories. Equivalent for real text.
//! - No echo guards (STT parroting the hint list, LLM duplicating
//!   output). Follow-up if bias loops ever appear.

use std::collections::HashMap;

/// Whisper-family decoder prompt budget (chars). The decoder reads
/// ~223 tokens and keeps the TAIL: over-budget lists still bias with
/// their end, never error.
pub const WHISPER_PROMPT_CHARS: usize = 900;
/// Groq rejects prompts over 896 chars; margin for counting drift.
pub const GROQ_PROMPT_CHARS: usize = 890;

/// Comma-joined hint list, cut at the last comma inside `max_chars`
/// so no entry ships half-spelled. Empty input → None (no prompt).
pub fn dictionary_prompt(words: &[String], max_chars: usize) -> Option<String> {
    let cleaned: Vec<&str> = words
        .iter()
        .map(|w| w.trim())
        .filter(|w| !w.is_empty())
        .collect();
    if cleaned.is_empty() {
        return None;
    }
    let full = cleaned.join(", ");
    if full.len() <= max_chars {
        return Some(full);
    }
    let head = &full[..max_chars];
    match head.rfind(',') {
        Some(i) => Some(head[..i].to_string()),
        None => Some(head.to_string()),
    }
}

fn fold_capital_i_dot(s: &str) -> String {
    s.replace('İ', "i")
}

fn fold_key(s: &str) -> String {
    fold_capital_i_dot(s).to_lowercase()
}

/// An uppercase I may mean Turkish ı as well as English i.
fn dotless_variant(key: &str) -> String {
    key.replace('I', "ı").to_lowercase()
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// One trigger, pre-folded for scanning.
struct Trigger {
    /// Lowercased folded pattern chars (with İ→i applied).
    pattern: Vec<char>,
    /// Whether each pattern char accepts the dotless reading.
    dotless_ok: Vec<bool>,
    replacement: String,
}

fn compile_trigger(trigger: &str, replacement: &str) -> Option<Trigger> {
    let folded = fold_capital_i_dot(trigger.trim());
    if folded.is_empty() {
        return None;
    }
    // Lowercase for matching; remember which positions came from an
    // ASCII 'i' so the [iİ]/[ıI] readings apply there.
    let lowered: Vec<char> = folded.chars().flat_map(|c| c.to_lowercase()).collect();
    let raw: Vec<char> = trigger.trim().chars().collect();
    let mut pattern = Vec::with_capacity(lowered.len());
    let mut dotless_ok = Vec::with_capacity(lowered.len());
    // Walk raw and lowered in lockstep (to_lowercase may expand İ,
    // already folded above, so lengths match for our purposes; fall
    // back to plain lowered on mismatch).
    let mut ri = 0;
    let raws: Vec<char> = raw;
    for c in lowered {
        let r = raws.get(ri).copied().unwrap_or('?');
        pattern.push(c);
        dotless_ok.push(r == 'i' || r == 'I');
        // Advance raw once per lowered char (approximate for
        // expansions; exact for ASCII + pre-folded text).
        ri += 1;
    }
    Some(Trigger {
        pattern,
        dotless_ok,
        replacement: replacement.to_string(),
    })
}

fn fold_text_char(c: char) -> char {
    // Mirror the trigger fold so both sides compare canonically.
    if c == 'İ' {
        'i'
    } else {
        c.to_lowercase().next().unwrap_or(c)
    }
}

fn char_matches(pat: char, dotless: bool, text: char) -> bool {
    let t = fold_text_char(text);
    if t == pat {
        return true;
    }
    // Turkish readings of an ASCII-i trigger position.
    if !dotless {
        return false;
    }
    (pat == 'i' && t == 'ı') || (pat == 'ı' && t == 'i')
}

/// Longest-first single pass. Returns the expanded text.
pub fn expand_snippets(text: &str, snippets: &[(String, String)]) -> String {
    let mut triggers: Vec<Trigger> = snippets
        .iter()
        .filter_map(|(t, r)| compile_trigger(t, r))
        .collect();
    if triggers.is_empty() {
        return text.to_string();
    }
    // Longest-first so "investor ask" wins over "ask".
    triggers.sort_by(|a, b| b.pattern.len().cmp(&a.pattern.len()));
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        // Boundary before: start or non-word char.
        let at_boundary = i == 0 || !is_word_char(chars[i - 1]);
        let mut hit: Option<&Trigger> = None;
        let mut hit_len = 0;
        if at_boundary {
            for tr in &triggers {
                let n = tr.pattern.len();
                if i + n > chars.len() {
                    continue;
                }
                let mut ok = true;
                for (k, (&p, &d)) in tr.pattern.iter().zip(tr.dotless_ok.iter()).enumerate() {
                    if !char_matches(p, d, chars[i + k]) {
                        ok = false;
                        break;
                    }
                }
                // Boundary after: end or non-word char.
                if ok && (i + n == chars.len() || !is_word_char(chars[i + n])) {
                    hit = Some(tr);
                    hit_len = n;
                    break; // longest-first: first hit wins
                }
            }
        }
        if let Some(tr) = hit {
            out.push_str(&tr.replacement);
            i += hit_len;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_joins_and_trims_at_comma() {
        let words = vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()];
        assert_eq!(
            dictionary_prompt(&words, 900),
            Some("alpha, beta, gamma".to_string())
        );
        // Budget inside "beta": cut at the last comma.
        assert_eq!(dictionary_prompt(&words, 10), Some("alpha".to_string()));
        assert_eq!(dictionary_prompt(&[], 900), None);
        assert_eq!(dictionary_prompt(&["  ".to_string()], 900), None);
    }

    #[test]
    fn snippets_longest_first_and_boundaries() {
        let snips = vec![
            ("ask".to_string(), "ASKED".to_string()),
            ("investor ask".to_string(), "INVESTOR ASKED".to_string()),
        ];
        assert_eq!(
            expand_snippets("the investor ask now", &snips),
            "the INVESTOR ASKED now"
        );
        // No inside-word matches.
        assert_eq!(expand_snippets("basket task", &snips), "basket task");
        // Case-insensitive.
        assert_eq!(expand_snippets("ASK me", &snips), "ASKED me");
        // Punctuation boundaries.
        assert_eq!(expand_snippets("ask, me!", &snips), "ASKED, me!");
    }

    #[test]
    fn snippets_turkish_dot() {
        let snips = vec![("imza".to_string(), "SIGN".to_string())];
        assert_eq!(expand_snippets("İmza here", &snips), "SIGN here");
    }

    #[test]
    fn snippets_empty_is_passthrough() {
        assert_eq!(expand_snippets("hello world", &[]), "hello world");
    }
}
