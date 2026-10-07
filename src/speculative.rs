//! Speculative streaming for non-streaming engines (whisper family).
//!
//! The model can't stream, so we do the streaming around it: while
//! the user speaks, decode the growing buffer in the background and
//! internally confirm stable prefixes (local agreement). On release
//! only the unconfirmed tail is decoded and merged. Nothing is ever
//! shown mid-hold; hypotheses stay internal until the final text.
//!
//! Rules that keep this honest:
//! - ONE inference at a time (single worker, next pass only after the
//!   previous lands). On slow machines passes space out by themselves
//!   instead of queueing stale audio behind a backlog.
//! - Agreement compares word prefixes minus a trailing unstable
//!   margin; only the overlap region is ever re-decoded.
//! - The tail always re-decodes with overlap; a failed merge falls
//!   back to a full decode (accuracy over saved milliseconds).
//! - Energy VAD (see [`crate::audio`]) marks phrase pauses, not a
//!   neural model: good enough for commit hints in quiet rooms.

/// Words at the hypothesis tail too unstable to confirm.
pub const UNSTABLE_TAIL_WORDS: usize = 3;
/// Cap on the overlap search when merging (words).
pub const MAX_OVERLAP_WORDS: usize = 12;

fn words(text: &str) -> Vec<&str> {
    text.split_whitespace().collect()
}

/// Longest common word prefix of two hypotheses, minus the unstable
/// tail margin. Returns the confirmed prefix (may be empty).
pub fn confirm_prefix(prev: &str, next: &str) -> String {
    let a = words(prev);
    let b = words(next);
    let mut n = 0;
    while n < a.len() && n < b.len() && a[n] == b[n] {
        n += 1;
    }
    let confirmed = n.saturating_sub(UNSTABLE_TAIL_WORDS);
    a[..confirmed].join(" ")
}

/// Merge committed text with a tail decode that overlaps it.
/// Finds the longest suffix-of-committed == prefix-of-tail match
/// (up to [`MAX_OVERLAP_WORDS`]); falls back to plain concatenation
/// when nothing overlaps (caller may full-decode instead).
pub fn merge_tail(committed: &str, tail: &str) -> String {
    let a = words(committed);
    let b = words(tail);
    if a.is_empty() {
        return tail.trim().to_string();
    }
    if b.is_empty() {
        return committed.trim().to_string();
    }
    let max = MAX_OVERLAP_WORDS.min(a.len()).min(b.len());
    let mut overlap = 0;
    for n in (1..=max).rev() {
        if a[a.len() - n..] == b[..n] {
            overlap = n;
            break;
        }
    }
    let mut out = a.join(" ");
    if overlap < b.len() {
        out.push(' ');
        out.push_str(&b[overlap..].join(" "));
    }
    out
}

/// Slice the unconfirmed audio window: everything from
/// `committed_len` samples, keeping `overlap` samples of confirmed
/// audio for context. All in 16 kHz samples.
pub fn unconfirmed_window(buffered_16k: &[f32], committed_len: usize, overlap: usize) -> Vec<f32> {
    let from = committed_len.saturating_sub(overlap.min(committed_len));
    buffered_16k[from..].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agreement_confirms_stable_prefix() {
        let p1 = "i want to create a new application that will";
        let p2 = "i want to create a new application which will transcribe";
        // Common prefix 7 words, minus 3 unstable.
        assert_eq!(confirm_prefix(p1, p2), "i want to create");
        assert_eq!(confirm_prefix(p1, "completely different words here"), "");
        assert_eq!(confirm_prefix("", "hello"), "");
    }

    #[test]
    fn merge_overlaps_or_concatenates() {
        assert_eq!(
            merge_tail("i want to create", "to create a new app"),
            "i want to create a new app"
        );
        assert_eq!(merge_tail("", "fresh start"), "fresh start");
        assert_eq!(merge_tail("done", ""), "done");
        // No overlap: plain concat.
        assert_eq!(merge_tail("hello world", "foo bar"), "hello world foo bar");
    }

    #[test]
    fn window_keeps_overlap() {
        let buf: Vec<f32> = (0..100).map(|i| i as f32).collect();
        let w = unconfirmed_window(&buf, 80, 10);
        assert_eq!(w.len(), 30);
        assert_eq!(w[0] as i32, 70);
        // Overlap larger than committed clamps to zero.
        let w = unconfirmed_window(&buf, 5, 50);
        assert_eq!(w.len(), 100);
    }
}
