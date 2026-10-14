//! Pre-postprocessing text norms: dictionary prompt bias + snippet
//! expansion. Port of OpenWhispr's `dictionaryPromptCap`
//! budgets and `snippets.ts` matcher (regex-free: equivalent
//! longest-first scan with the same boundary rules).
//!
//! Pipeline position: triggers are recognized on the ORIGINAL
//! transcription, BEFORE the LLM cleanup (`finalize_transcript` in
//! [`crate::transcribe`]). Each matched span is swapped for a
//! per-utterance placeholder whose saved replacement is kept outside
//! the LLM request; after cleanup the placeholders are validated and
//! restored verbatim, never re-matched. Consequences:
//! - The LLM can neither rewrite a valid trigger (matching already
//!   ran) nor modify a saved replacement (it never sees one).
//! - No recursive substitution by construction: restored
//!   replacements are spliced literally, and the deterministic
//!   fallback path expands the original text exactly once.
//! - A failed contract (missing, altered, duplicated, reordered, or
//!   unexpected placeholders — including malformed reserved-marker
//!   fragments) discards the cleanup response and falls back to the
//!   once-expanded original transcription. Input already resembling
//!   markers skips cleanup up front, so user text survives verbatim.
//!
//! Matching is case- AND accent-insensitive on both sides, with one
//! shared fold (`fold_char`/`norm_string`): `Ó`/`ó`/`O`/`o` all match,
//! `İ`/`I`/`ı`/`i` all match. Decomposed combining marks are skipped
//! so `o` + U+0301 matches `ó`. Boundary = start/end or
//! non-(alphanumeric/`_`); the reference uses Unicode P/S
//! categories. Equivalent for real text.
//!
//! Supported folding (snippet triggers only):
//! - Latin accents fold to their bare base: `é`→`e`, `ó`→`o`,
//!   `ü`→`u`, `ç`→`c`, and so on through the table in `fold_char`.
//! - `ñ` folds with `n` (likewise `ń`/`ņ`/`ň`): trigger `cana`
//!   matches text `caña` and vice versa. This is a deliberate
//!   fuzzy-match tradeoff, not a linguistic claim: `ñ` is its own
//!   letter in Spanish, but for short dictated triggers an accent-
//!   blind match misses fewer invocations. The spelling dictionary
//!   below keeps `n`/`ñ` distinct.
//! - Turkish dotted/dotless `İ`/`I`/`ı`/`i` all read as `i`, on both
//!   the trigger and the transcript side.
//! - `ß`/`æ`/`œ` stay distinct (their casefolds expand to `ss`/`ae`/
//!   `oe` and can't map 1:1 back to the original span).
//!
//! Two identities, two functions: snippet-trigger matching is fuzzy
//! (`norm_string`: case- and accent-insensitive), while spelling-
//! dictionary identity is exact on accents (`dict_key`:
//! case-insensitive, canonical composed/decomposed forms unified,
//! but `si` ≠ `sí` and `papa` ≠ `papá`).
//!
//! Deliberate simplifications vs the reference:
//! - Snippet fuzzy matching uses no `unicode-normalization` crate:
//!   precomposed Latin accents fold via table, decomposed marks via
//!   combining-range strip. Rare scripts outside the table compare
//!   case-insensitively only. (Spelling-dictionary identity below
//!   does use the crate for complete NFC.)
//! - 1:1 char folding: `ß`/`æ`/`œ` stay distinct (no `ss`/`ae`
//!   expansion) so match spans map back to the original text.
//! - No echo guards (STT parroting the hint list, LLM duplicating
//!   output). Follow-up if bias loops ever appear.

use unicode_normalization::UnicodeNormalization;

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
    let mut end = max_chars.min(full.len());
    while !full.is_char_boundary(end) {
        end -= 1;
    }
    let head = &full[..end];
    match head.rfind(',') {
        Some(i) => Some(head[..i].to_string()),
        None => Some(head.to_string()),
    }
}

fn fold_char(c: char) -> char {
    // Canonical fold for snippet-trigger matching and snippet
    // duplicate detection: lowercase + Latin accent strip + Turkish
    // dot fold (İ/I/ı/i all read as 'i', symmetric on both sides).
    if c == 'İ' {
        return 'i';
    }
    let l = c.to_lowercase().next().unwrap_or(c);
    match l {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' | 'ǎ' | 'ȁ' | 'ȃ' | 'ạ' | 'ả' | 'ấ'
        | 'ầ' | 'ẩ' | 'ẫ' | 'ậ' | 'ắ' | 'ằ' | 'ẳ' | 'ẵ' | 'ặ' => 'a',
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => 'c',
        'ď' | 'đ' | 'ð' => 'd',
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' | 'ẹ' | 'ẻ' | 'ẽ' | 'ế' | 'ề' | 'ể'
        | 'ễ' | 'ệ' => 'e',
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => 'g',
        'ĥ' | 'ħ' => 'h',
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' | 'ǐ' | 'ȉ' | 'ȋ' | 'ị' | 'ỉ' => {
            'i'
        }
        'ĵ' => 'j',
        'ķ' => 'k',
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => 'l',
        'ñ' | 'ń' | 'ņ' | 'ň' | 'ŉ' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' | 'ǒ' | 'ȍ' | 'ȏ' | 'ọ' | 'ỏ' | 'ố'
        | 'ồ' | 'ổ' | 'ỗ' | 'ộ' | 'ớ' | 'ờ' | 'ở' | 'ỡ' | 'ợ' | 'ơ' => 'o',
        'ŕ' | 'ŗ' | 'ř' => 'r',
        'ś' | 'ŝ' | 'ş' | 'š' | 'ș' => 's',
        'ţ' | 'ť' | 'ŧ' | 'ț' | 'þ' => 't',
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' | 'ǔ' | 'ȕ' | 'ȗ' | 'ụ' | 'ủ'
        | 'ứ' | 'ừ' | 'ử' | 'ữ' | 'ự' | 'ư' => 'u',
        'ŵ' => 'w',
        'ý' | 'ÿ' | 'ŷ' | 'ỳ' | 'ỵ' | 'ỷ' | 'ỹ' => 'y',
        'ź' | 'ż' | 'ž' => 'z',
        // ß/æ/œ stay distinct: their casefolds expand (ss/ae/oe) and
        // can't map 1:1 back to the original span.
        _ => l,
    }
}

fn is_combining(c: char) -> bool {
    matches!(c,
        '\u{0300}'..='\u{036F}'
        | '\u{1AB0}'..='\u{1AFF}'
        | '\u{1DC0}'..='\u{1DFF}'
        | '\u{20D0}'..='\u{20FF}'
        | '\u{FE20}'..='\u{FE2F}')
}

/// Normalized comparison form for snippet triggers: combining marks
/// dropped, the rest folded. Shared by expansion matching and
/// snippet duplicate detection so both agree on what "same trigger"
/// means.
pub fn norm_string(s: &str) -> String {
    s.chars()
        .filter(|c| !is_combining(*c))
        .map(fold_char)
        .collect()
}

/// Spelling-dictionary identity: NFC canonical normalization plus
/// full lowercase, so canonically equivalent spellings share one key
/// while accents stay meaningful. `si` and `sí` (likewise `papa`/
/// `papá`) are distinct entries; `CAMIÓN`, `camión`, and `camio` +
/// U+0301 + `n` are the same entry. Vietnamese stacks (`ế` =
/// `e` + U+0302 + U+0301), dot-below letters (`ạ` = `a` + U+0323),
/// and equivalent mark orderings all unify through NFC.
/// Lowercasing keeps its full output (`İ` → `i` + U+0307, not just
/// `i`); the retained Turkish fold then reads that dotted-`i`
/// sequence as plain `i`, so composed `İ` and decomposed `I` + U+0307
/// agree. Dotless `ı` is untouched: `i`/`ı` stay distinct letters.
pub fn dict_key(s: &str) -> String {
    s.trim()
        .nfc()
        .collect::<String>()
        .to_lowercase()
        .replace("i\u{307}", "i")
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// One trigger, pre-folded for scanning.
struct Trigger {
    /// Folded pattern chars.
    pattern: Vec<char>,
    replacement: String,
}

fn compile_trigger(trigger: &str, replacement: &str) -> Option<Trigger> {
    let pattern: Vec<char> = norm_string(trigger.trim()).chars().collect();
    if pattern.is_empty() {
        return None;
    }
    Some(Trigger {
        pattern,
        replacement: replacement.to_string(),
    })
}

/// One recognized trigger occurrence: byte span in the original
/// text plus the saved replacement for that span.
pub struct SnippetMatch {
    /// Byte offset where the matched span starts.
    pub start: usize,
    /// Byte offset where it ends (exclusive), trailing combining
    /// marks attached to its last base char included.
    pub end: usize,
    /// Literal replacement text for the span.
    pub replacement: String,
}

/// Longest-first scan over the normalized view. Matching runs in
/// normalized space (case/accent-insensitive); each hit records the
/// ORIGINAL byte span so output keeps its own casing. Returns
/// non-overlapping matches left to right.
pub fn find_snippet_matches(text: &str, snippets: &[(String, String)]) -> Vec<SnippetMatch> {
    let mut triggers: Vec<Trigger> = snippets
        .iter()
        .filter_map(|(t, r)| compile_trigger(t, r))
        .collect();
    if triggers.is_empty() {
        return Vec::new();
    }
    // Longest-first so "investor ask" wins over "ask".
    triggers.sort_by_key(|trigger| std::cmp::Reverse(trigger.pattern.len()));
    let chars: Vec<char> = text.chars().collect();
    // Normalized view + back-map to original char indices
    // (combining marks contribute no entry).
    let mut norm: Vec<char> = Vec::with_capacity(chars.len());
    let mut back: Vec<usize> = Vec::with_capacity(chars.len());
    for (i, &c) in chars.iter().enumerate() {
        if is_combining(c) {
            continue;
        }
        norm.push(fold_char(c));
        back.push(i);
    }
    if norm.is_empty() {
        return Vec::new();
    }
    let byte_of: Vec<usize> = text.char_indices().map(|(b, _)| b).collect();
    let mut out = Vec::new();
    let mut ni = 0;
    while ni < norm.len() {
        // Boundary before: start or non-word char (normalized side,
        // so decomposed accents don't split words).
        let at_boundary = ni == 0 || !is_word_char(norm[ni - 1]);
        let mut hit: Option<usize> = None;
        let mut hit_len = 0;
        if at_boundary {
            for (idx, tr) in triggers.iter().enumerate() {
                let n = tr.pattern.len();
                if ni + n > norm.len() {
                    continue;
                }
                if norm[ni..ni + n] != tr.pattern[..] {
                    continue;
                }
                // Boundary after: end or non-word char.
                if ni + n == norm.len() || !is_word_char(norm[ni + n]) {
                    hit = Some(idx);
                    hit_len = n;
                    break; // longest-first: first hit wins
                }
            }
        }
        if let Some(idx) = hit {
            // Byte span of the original text, swallowing trailing
            // combining marks attached to its last base char.
            let mut end_char = back[ni + hit_len - 1] + 1;
            while end_char < chars.len() && is_combining(chars[end_char]) {
                end_char += 1;
            }
            out.push(SnippetMatch {
                start: byte_of[back[ni]],
                end: byte_of.get(end_char).copied().unwrap_or(text.len()),
                replacement: triggers[idx].replacement.clone(),
            });
            ni += hit_len;
        } else {
            ni += 1;
        }
    }
    out
}

/// Longest-first single pass. Matching runs in normalized space
/// (case/accent-insensitive); replacements splice the ORIGINAL text
/// spans so output keeps its own casing. Inserted replacements are
/// never rescanned: self-containing replacements (`go` → `go now`)
/// or replacements naming other triggers expand exactly once.
/// Returns the expanded text.
pub fn expand_snippets(text: &str, snippets: &[(String, String)]) -> String {
    let matches = find_snippet_matches(text, snippets);
    if matches.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for m in &matches {
        out.push_str(&text[cursor..m.start]);
        out.push_str(&m.replacement);
        cursor = m.end;
    }
    out.push_str(&text[cursor..]);
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
        // Symmetric: dotless trigger matches dotted text and back.
        let snips = vec![("kırmızı".to_string(), "RED".to_string())];
        assert_eq!(expand_snippets("KIRMIZI here", &snips), "RED here");
        let snips = vec![("KIRMIZI".to_string(), "RED".to_string())];
        assert_eq!(expand_snippets("kırmızı here", &snips), "RED here");
    }

    #[test]
    fn snippets_accent_and_case_insensitive() {
        let snips = vec![("como estas".to_string(), "HOW ARE YOU".to_string())];
        // Capitals + precomposed accents match a plain trigger.
        assert_eq!(expand_snippets("¿Cómo estás?", &snips), "¿HOW ARE YOU?");
        let snips = vec![("Óscar".to_string(), "OSCAR!".to_string())];
        assert_eq!(expand_snippets("oscar llegó", &snips), "OSCAR! llegó");
        assert_eq!(expand_snippets("ÓSCAR llegó", &snips), "OSCAR! llegó");
        // Accented trigger matches plain transcription.
        let snips = vec![("acción".to_string(), "ACTION".to_string())];
        assert_eq!(
            expand_snippets("la ACCION ahora", &snips),
            "la ACTION ahora"
        );
        // ñ folds with n on both sides.
        let snips = vec![("cana".to_string(), "REED".to_string())];
        assert_eq!(expand_snippets("la caña alta", &snips), "la REED alta");
        // Decomposed (o + combining acute) matches precomposed.
        let snips = vec![("café".to_string(), "COFFEE".to_string())];
        assert_eq!(expand_snippets("cafe\u{301} time", &snips), "COFFEE time");
        // Inside-word still never matches, accented or not.
        assert_eq!(expand_snippets("basket task", &snips), "basket task");
        assert_eq!(expand_snippets("acafés here", &snips), "acafés here");
        // Surrounding text keeps its own casing/accents.
        let snips = vec![("hola".to_string(), "HI".to_string())];
        assert_eq!(
            expand_snippets("Él dijo HOLA, ¿qué tal?", &snips),
            "Él dijo HI, ¿qué tal?"
        );
    }

    #[test]
    fn snippets_never_expand_recursively() {
        // Self-containing replacement: one original trigger, one
        // insertion. A pipeline that rescans replacements would
        // yield "go now now".
        let snips = vec![("go".to_string(), "go now".to_string())];
        assert_eq!(expand_snippets("go", &snips), "go now");
        assert_eq!(expand_snippets("go go", &snips), "go now go now");
        // Replacement naming another trigger stays literal.
        let snips = vec![
            ("signature".to_string(), "Regards, Oscar".to_string()),
            ("Oscar".to_string(), "ANOTHER SNIPPET".to_string()),
        ];
        assert_eq!(expand_snippets("signature", &snips), "Regards, Oscar");
        // A genuine original trigger still expands.
        assert_eq!(
            expand_snippets("signature from Oscar", &snips),
            "Regards, Oscar from ANOTHER SNIPPET"
        );
    }

    #[test]
    fn dict_key_keeps_accents_but_unifies_case_and_canonical_forms() {
        // Meaningful accent distinctions survive.
        assert_ne!(dict_key("si"), dict_key("sí"));
        assert_ne!(dict_key("papa"), dict_key("papá"));
        // Case does not create a new entry.
        assert_eq!(dict_key("CAMIÓN"), dict_key("camión"));
        assert_eq!(dict_key("  MyWhisper "), dict_key("mywhisper"));
        // Composed and decomposed spellings are one entry.
        assert_eq!(dict_key("café"), dict_key("cafe\u{301}"));
        assert_eq!(dict_key("caña"), dict_key("can\u{303}a"));
        // Stacked marks beyond the Latin-1 range unify too.
        assert_eq!(dict_key("ế"), dict_key("e\u{302}\u{301}"));
        assert_eq!(dict_key("ǘ"), dict_key("u\u{308}\u{301}"));
        // Below-base marks unify, in either input order.
        assert_eq!(dict_key("ạ"), dict_key("a\u{323}"));
        assert_eq!(dict_key("ẹ"), dict_key("e\u{323}"));
        assert_eq!(dict_key("o\u{323}\u{301}"), dict_key("o\u{301}\u{323}"));
        // Turkish İ folds to i; dotless ı stays its own letter.
        assert_eq!(dict_key("İMZA"), dict_key("imza"));
        assert_eq!(dict_key("İ"), dict_key("I\u{307}"));
        assert_ne!(dict_key("kırmızı"), dict_key("kirmizi"));
        // Case and decomposition combine: capitals match the
        // decomposed lowercase spelling.
        assert_eq!(dict_key("Ế"), dict_key("e\u{302}\u{301}"));
        assert_eq!(dict_key("CAFÉ"), dict_key("cafe\u{301}"));
    }

    #[test]
    fn snippets_empty_is_passthrough() {
        assert_eq!(expand_snippets("hello world", &[]), "hello world");
    }
}
