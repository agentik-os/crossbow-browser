//! Fractional ordering keys.
//!
//! Sidebar order is stored as base-62 strings that sort lexicographically, so
//! moving one item rewrites only that item. Concurrent moves on two devices
//! converge through the normal per-entity last-writer-wins rule.

const DIGITS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

fn digit_index(c: u8) -> usize {
    DIGITS.iter().position(|d| *d == c).expect("order keys only contain base-62 digits")
}

/// A key strictly between `a` and `b` (`None` means the start / end of the list).
///
/// Panics in debug builds if `a >= b`.
pub fn key_between(a: Option<&str>, b: Option<&str>) -> String {
    let a = a.unwrap_or("");
    debug_assert!(b.is_none_or(|b| a < b), "key_between({a:?}, {b:?}) needs a < b");
    debug_assert!(!a.ends_with('0') && !b.unwrap_or("").ends_with('0'));
    midpoint(a.as_bytes(), b.map(str::as_bytes))
}

fn midpoint(a: &[u8], b: Option<&[u8]>) -> String {
    if let Some(b) = b {
        // Shared prefix (a padded with zeros) is copied as-is.
        let mut n = 0;
        while n < b.len() && a.get(n).copied().unwrap_or(b'0') == b[n] {
            n += 1;
        }
        if n > 0 {
            let rest_a = if n <= a.len() { &a[n..] } else { &[][..] };
            let mut out = String::from_utf8(b[..n].to_vec()).unwrap();
            out.push_str(&midpoint(rest_a, Some(&b[n..])));
            return out;
        }
    }
    let da = a.first().map(|c| digit_index(*c)).unwrap_or(0);
    let db = b.and_then(|b| b.first()).map(|c| digit_index(*c)).unwrap_or(DIGITS.len());
    if db - da > 1 {
        let mid = (da + db).div_ceil(2);
        return (DIGITS[mid] as char).to_string();
    }
    // Consecutive first digits.
    if let Some(b) = b {
        if b.len() > 1 {
            return (b[0] as char).to_string();
        }
    }
    let mut out = (DIGITS[da] as char).to_string();
    out.push_str(&midpoint(a.get(1..).unwrap_or(&[]), None));
    out
}

/// `n` evenly spread keys for a fresh list.
pub fn initial_keys(n: usize) -> Vec<String> {
    let mut keys = Vec::with_capacity(n);
    let mut prev: Option<String> = None;
    for _ in 0..n {
        let k = key_between(prev.as_deref(), None);
        keys.push(k.clone());
        prev = Some(k);
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_sort_between_neighbours() {
        let first = key_between(None, None);
        let after = key_between(Some(&first), None);
        let before = key_between(None, Some(&first));
        let mid = key_between(Some(&first), Some(&after));
        assert!(before < first && first < mid && mid < after, "{before} {first} {mid} {after}");
    }

    #[test]
    fn repeated_inserts_at_front_and_between_stay_ordered() {
        let mut keys = initial_keys(3);
        for _ in 0..200 {
            let k = key_between(None, Some(&keys[0]));
            keys.insert(0, k);
        }
        for _ in 0..200 {
            let k = key_between(Some(&keys[10]), Some(&keys[11]));
            keys.insert(11, k);
        }
        for _ in 0..200 {
            let k = key_between(Some(keys.last().unwrap()), None);
            keys.push(k);
        }
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(sorted, keys);
        sorted.dedup();
        assert_eq!(sorted.len(), keys.len());
        assert!(keys.iter().all(|k| !k.ends_with('0')));
    }
}
