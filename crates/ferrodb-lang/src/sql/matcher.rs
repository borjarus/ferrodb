//! Ranking of completion candidates against the typed prefix.
//!
//! In order: prefix (`ord` → `orders`), segment abbreviation
//! (`oi` → `order_items`, `custId` → `customer_id`), substring, then any
//! subsequence. A prefix typed in a Cyrillic keyboard layout by mistake
//! (`щквукы` for `orders`) is translated to the Latin keys and matched
//! again with a small penalty.

/// Score of `candidate` for `typed` (higher is better); `None` = no match.
pub fn score(candidate: &str, typed: &str) -> Option<i32> {
    if typed.is_empty() {
        return Some(0);
    }
    if let Some(s) = score_plain(candidate, typed) {
        return Some(s);
    }
    let latin = from_cyrillic_layout(typed)?;
    score_plain(candidate, &latin).map(|s| s - 150)
}

fn score_plain(candidate: &str, typed: &str) -> Option<i32> {
    let c: Vec<char> = candidate.chars().flat_map(char::to_lowercase).collect();
    let t: Vec<char> = typed.chars().flat_map(char::to_lowercase).collect();
    if t.len() > c.len() {
        return None;
    }
    let extra = (c.len() - t.len()).min(100) as i32;
    if c.starts_with(&t) {
        return Some(1000 - extra);
    }
    let original: Vec<char> = candidate.chars().collect();
    if original.len() == c.len() && humps(&original, &t) {
        return Some(700 - extra);
    }
    if c.windows(t.len()).any(|w| w == t.as_slice()) {
        return Some(400 - extra);
    }
    let mut it = c.iter();
    if t.iter().all(|x| it.any(|y| y == x)) {
        return Some(200 - extra);
    }
    None
}

/// Starts of the segments of an identifier: after `_`, `.`, ` `, at an
/// upper-case letter following a lower-case one, at a digit run.
fn segment_starts(c: &[char]) -> Vec<bool> {
    let mut v = vec![false; c.len()];
    for i in 0..c.len() {
        v[i] = i == 0
            || matches!(c[i - 1], '_' | '.' | ' ' | '-' | '$')
            || (c[i].is_uppercase() && c[i - 1].is_lowercase())
            || (c[i].is_ascii_digit() && !c[i - 1].is_ascii_digit());
    }
    v
}

/// `typed` matches by taking a prefix of successive segments.
fn humps(candidate: &[char], typed: &[char]) -> bool {
    let lower: Vec<char> = candidate
        .iter()
        .map(|c| c.to_lowercase().next().unwrap_or(*c))
        .collect();
    let starts = segment_starts(candidate);
    if typed.is_empty() || lower.first() != typed.first() {
        return false;
    }
    // DP over (typed index, candidate index) of the last matched char.
    let n = lower.len();
    let mut reach = vec![false; n];
    reach[0] = true;
    for &tc in &typed[1..] {
        let mut next = vec![false; n];
        for (ci, &r) in reach.iter().enumerate() {
            if !r {
                continue;
            }
            // Continue inside the segment.
            if ci + 1 < n && lower[ci + 1] == tc && !starts[ci + 1] {
                next[ci + 1] = true;
            }
            // Jump to a later segment start.
            for (k, &s) in starts.iter().enumerate().skip(ci + 1) {
                if s && lower[k] == tc {
                    next[k] = true;
                }
            }
        }
        if !next.iter().any(|x| *x) {
            return false;
        }
        reach = next;
    }
    true
}

/// Keys of a Russian/Ukrainian (ЙЦУКЕН) layout → the US keys under them;
/// `None` when nothing was Cyrillic.
pub fn from_cyrillic_layout(s: &str) -> Option<String> {
    let mut any = false;
    let out: String = s
        .chars()
        .map(|c| {
            let l = c.to_lowercase().next().unwrap_or(c);
            let m = match l {
                'й' => 'q',
                'ц' => 'w',
                'у' => 'e',
                'к' => 'r',
                'е' => 't',
                'н' => 'y',
                'г' => 'u',
                'ш' => 'i',
                'щ' => 'o',
                'з' => 'p',
                'х' => '[',
                'ъ' | 'ї' => ']',
                'ф' => 'a',
                'ы' | 'і' => 's',
                'в' => 'd',
                'а' => 'f',
                'п' => 'g',
                'р' => 'h',
                'о' => 'j',
                'л' => 'k',
                'д' => 'l',
                'ж' => ';',
                'э' | 'є' => '\'',
                'я' => 'z',
                'ч' => 'x',
                'с' => 'c',
                'м' => 'v',
                'и' => 'b',
                'т' => 'n',
                'ь' => 'm',
                'б' => ',',
                'ю' => '.',
                'ё' => '`',
                _ => return c,
            };
            any = true;
            m
        })
        .collect();
    any.then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranking_order() {
        let p = score("orders", "ord").unwrap();
        let h = score("order_items", "oi").unwrap();
        let s = score("customer_orders", "orders").unwrap();
        let f = score("order_items", "oits").unwrap();
        assert!(p > h && h > s && s > f, "{p} {h} {s} {f}");
        assert!(score("orders", "xyz").is_none());
        assert!(score("customer_id", "cuid").is_some());
        assert!(score("customerId", "cI").is_some());
        // Shorter candidates win among prefixes.
        assert!(score("order", "ord").unwrap() > score("orders", "ord").unwrap());
    }

    #[test]
    fn wrong_keyboard_layout() {
        assert_eq!(from_cyrillic_layout("ыудусе").as_deref(), Some("select"));
        assert!(score("orders", "щкв").is_some());
        assert!(score("orders", "щкв").unwrap() < score("orders", "ord").unwrap());
        assert_eq!(from_cyrillic_layout("abc"), None);
    }
}
