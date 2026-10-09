//! Pre-postprocessing text norms: dictionary prompt bias + snippet
//! expansion. Port of OpenWhispr's `dictionaryPromptCap`
//! budgets and `snippets.ts` matcher (regex-free: equivalent
//! longest-first scan with the same boundary rules).
//!
//! Scope is deliberately pre-LLM: STT bias goes out with the next
//! inference, snippet expansion rewrites the transcript before
//! `post_process`. The LLM stage receives the expanded text; a
//! second expansion pass after cleanup re-inserts anything the
//! cleanup normalized back into a trigger (replacements stay
//! literal: the matcher never rescans inserted text).
//!
//! Matching is case- AND accent-insensitive on both sides, with one
//! shared fold (`fold_char`/`norm_string`): `Ó`/`ó`/`O`/`o` all match,
//! `İ`/`I`/`ı`/`i` all match. Decomposed combining marks are skipped
//! so `o` + U+0301 matches `ó`. Boundary = start/end or
//! non-(alphanumeric/`_`); the reference uses Unicode P/S
//! categories. Equivalent for real text.
//!
//! Deliberate simplifications vs the reference:
//! - No `unicode-normalization` crate: precomposed Latin accents fold
//!   via table, decomposed marks via combining-range strip. Rare
//!   scripts outside the table compare case-insensitively only.
//! - 1:1 char folding: `ß`/`æ`/`œ` stay distinct (no `ss`/`ae`
//!   expansion) so match spans map back to the original text.
//! - No echo guards (STT parroting the hint list, LLM duplicating
//!   output). Follow-up if bias loops ever appear.

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

fn fold_char(c: char) -> char {
    // Canonical fold shared by triggers, transcript matching and
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

/// Normalized comparison form: combining marks dropped, the rest
/// folded. Shared by expansion matching and duplicate detection so
/// both agree on what "same trigger" means.
pub fn norm_string(s: &str) -> String {
    s.chars()
        .filter(|c| !is_combining(*c))
        .map(fold_char)
        .collect()
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

/// Longest-first single pass. Matching runs in normalized space
/// (case/accent-insensitive); replacements splice the ORIGINAL text
/// spans so output keeps its own casing. Returns the expanded text.
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
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut ni = 0;
    // Original-char cursor: everything before it is emitted.
    let mut oc = if back[0] > 0 {
        for k in 0..back[0] {
            out.push(chars[k]);
        }
        back[0]
    } else {
        0
    };
    while ni < norm.len() {
        // Boundary before: start or non-word char (normalized side,
        // so decomposed accents don't split words).
        let at_boundary = ni == 0 || !is_word_char(norm[ni - 1]);
        let mut hit: Option<&Trigger> = None;
        let mut hit_len = 0;
        if at_boundary {
            for tr in &triggers {
                let n = tr.pattern.len();
                if ni + n > norm.len() {
                    continue;
                }
                if norm[ni..ni + n] != tr.pattern[..] {
                    continue;
                }
                // Boundary after: end or non-word char.
                if ni + n == norm.len() || !is_word_char(norm[ni + n]) {
                    hit = Some(tr);
                    hit_len = n;
                    break; // longest-first: first hit wins
                }
            }
        }
        if let Some(tr) = hit {
            out.push_str(&tr.replacement);
            // Skip the original span, swallowing trailing combining
            // marks attached to its last base char.
            let mut end = back[ni + hit_len - 1] + 1;
            while end < chars.len() && is_combining(chars[end]) {
                end += 1;
            }
            oc = end;
            ni += hit_len;
        } else {
            let next_oc = if ni + 1 < back.len() {
                back[ni + 1]
            } else {
                chars.len()
            };
            for k in oc..next_oc {
                out.push(chars[k]);
            }
            oc = next_oc;
            ni += 1;
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
        let snips = vec![("ano".to_string(), "YEAR".to_string())];
        assert_eq!(expand_snippets("el año pasado", &snips), "el YEAR pasado");
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
    fn snippets_empty_is_passthrough() {
        assert_eq!(expand_snippets("hello world", &[]), "hello world");
    }
}
