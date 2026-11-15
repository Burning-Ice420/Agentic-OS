//! Pure logic for life threads: the kernel routes every task and note into a
//! thread (work, home, health, ...), holds reminders from other threads while
//! you focus on one, and never sends items in a private thread to the model.
//! Only `core` + `alloc`, so the host crate unit-tests this exact file.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write;

/// Where an item goes when no rule claims it.
pub const INBOX: &str = "inbox";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thread {
    pub name: String,
    pub keywords: Vec<String>,
    /// Items in a private thread are never sent to the model.
    pub private: bool,
}

impl Thread {
    /// Blob encoding: `private|kw1 kw2` or `public|kw1 kw2`.
    pub fn encode(&self) -> String {
        let mut s = String::from(if self.private { "private|" } else { "public|" });
        s.push_str(&self.keywords.join(" "));
        s
    }
    pub fn decode(name: &str, s: &str) -> Option<Thread> {
        let mut it = s.splitn(2, '|');
        let private = match it.next()? { "private" => true, "public" => false, _ => return None };
        let keywords = it.next().unwrap_or("").split(' ').filter_map(norm_word).collect();
        Some(Thread { name: name.to_string(), keywords, private })
    }
}

/// Thread names: 1 to 12 chars, lowercase letters, digits or '-'; not reserved.
pub fn valid_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 12 && s != INBOX && !s.starts_with('-')
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Lowercase a word and strip surrounding punctuation; None if nothing is left.
pub fn norm_word(w: &str) -> Option<String> {
    let t: String = w.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '#')
        .chars().map(|c| c.to_ascii_lowercase()).collect();
    if t.is_empty() { None } else { Some(t) }
}

/// What the kernel's rules decided for a piece of text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuleRoute {
    /// The text named a thread with `#name`.
    Tagged(String),
    /// One thread's keywords matched more than any other's.
    Keyword { thread: String, word: String },
    /// Two or more threads tied on keyword matches.
    Ambiguous(Vec<String>),
    NoMatch,
}

/// Route text by the user's own rules: an explicit `#thread` wins, then the
/// thread with the most distinct keyword hits. Ties are not guessed.
pub fn route_by_rules(text: &str, threads: &[Thread]) -> RuleRoute {
    let words: Vec<String> = text.split_whitespace().filter_map(norm_word).collect();
    for w in &words {
        if let Some(tag) = w.strip_prefix('#') {
            if threads.iter().any(|t| t.name == tag) {
                return RuleRoute::Tagged(tag.to_string());
            }
        }
    }
    let mut best: Vec<(usize, &Thread, &str)> = Vec::new();
    for t in threads {
        let hits: Vec<&str> = t.keywords.iter()
            .filter(|k| words.iter().any(|w| w == *k || (w.len() == k.len() + 1 && w.starts_with(k.as_str()) && w.ends_with('s'))))
            .map(|k| k.as_str()).collect();
        if hits.is_empty() { continue; }
        let n = hits.len();
        match best.first() {
            Some((m, _, _)) if n < *m => {}
            Some((m, _, _)) if n == *m => best.push((n, t, hits[0])),
            _ => { best.clear(); best.push((n, t, hits[0])); }
        }
    }
    match best.len() {
        0 => RuleRoute::NoMatch,
        1 => RuleRoute::Keyword { thread: best[0].1.name.clone(), word: best[0].2.to_string() },
        _ => RuleRoute::Ambiguous(best.iter().map(|b| b.1.name.clone()).collect()),
    }
}

/// Read a thread name out of a model answer. Only names that exist count;
/// anything else is rejected (the caller falls back to the inbox).
pub fn parse_model_thread(answer: &str, threads: &[Thread]) -> Option<String> {
    answer.split(|c: char| !c.is_ascii_alphanumeric() && c != '-')
        .map(|w| w.to_ascii_lowercase())
        .find(|w| threads.iter().any(|t| &t.name == w))
}

/// The thread an item is in, deciding who to believe. A label written by a
/// trusted principal is used. A label that came from an untrusted peer is
/// ignored for a trusted item (a peer must not be able to move your private
/// note into a public thread); the rules on the item's own text decide instead.
pub fn effective_thread(text: &str, label: Option<(&str, bool)>, item_tainted: bool,
                        threads: &[Thread]) -> String {
    if let Some((l, label_tainted)) = label {
        let known = l == INBOX || threads.iter().any(|t| t.name == l);
        if known && (!label_tainted || item_tainted) {
            return l.to_string();
        }
    }
    match route_by_rules(text, threads) {
        RuleRoute::Tagged(t) | RuleRoute::Keyword { thread: t, .. } => t,
        _ => INBOX.to_string(),
    }
}

/// Is `k` an item key (`t3`, `n12`) rather than a derived one (`n3_tags`)?
pub fn is_item_key(k: &str) -> bool {
    k.len() > 1 && (k.starts_with('t') || k.starts_with('n')) && k[1..].bytes().all(|b| b.is_ascii_digit())
}

/// Keys of a node to withhold from the model: every item in a private thread,
/// plus its derived `_tags` and `_thread` keys. `items` are
/// (key, text, item_tainted, label) for each item key in the node.
pub fn private_keys(items: &[(String, String, bool, Option<(String, bool)>)], threads: &[Thread]) -> Vec<String> {
    let mut out = Vec::new();
    for (k, text, tainted, label) in items {
        let lane = effective_thread(text, label.as_ref().map(|(l, t)| (l.as_str(), *t)), *tainted, threads);
        if threads.iter().any(|t| t.private && t.name == lane) {
            out.push(k.clone());
            let mut a = k.clone(); a.push_str("_tags"); out.push(a);
            let mut b = k.clone(); b.push_str("_thread"); out.push(b);
        }
    }
    out
}

// ── Focus ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Focus {
    pub thread: String,
    /// Absolute minute when focus ends; 0 = until turned off.
    pub until: i64,
}

impl Focus {
    pub fn encode(&self) -> String {
        let mut s = self.thread.clone();
        let _ = write!(s, "|{}", self.until);
        s
    }
    pub fn decode(s: &str) -> Option<Focus> {
        let mut it = s.splitn(2, '|');
        let thread = it.next()?.to_string();
        let until = it.next()?.parse().ok()?;
        if thread.is_empty() { return None; }
        Some(Focus { thread, until })
    }
    pub fn expired(&self, now: i64) -> bool {
        self.until != 0 && now >= self.until
    }
}

/// May a reminder from `thread` interrupt now?
pub fn may_interrupt(focus: Option<&Focus>, thread: &str, now: i64) -> bool {
    match focus {
        None => true,
        Some(f) => f.expired(now) || f.thread == thread,
    }
}

/// Held reminder keys are stored space-separated.
pub fn held_decode(s: &str) -> Vec<String> {
    s.split(' ').filter(|k| !k.is_empty()).map(|k| k.to_string()).collect()
}

pub fn held_encode(keys: &[String]) -> String {
    keys.join(" ")
}
