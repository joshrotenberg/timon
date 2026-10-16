//! "Did you mean" suggestions for unknown names.

/// The candidate closest to `name`, if one is close enough to be a likely typo.
///
/// Close enough means an edit distance (with transpositions) of at most a third of the name's
/// length, and at least 1. Ties go to the alphabetically first candidate, so suggestions are
/// stable.
pub fn closest<'a>(name: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let limit = (name.chars().count() / 3).max(1);
    let mut best: Option<(usize, &str)> = None;
    for candidate in candidates {
        if candidate == name {
            continue;
        }
        let d = distance(name, candidate);
        if d > limit {
            continue;
        }
        best = match best {
            Some((bd, bc)) if bd < d || (bd == d && bc <= candidate) => Some((bd, bc)),
            _ => Some((d, candidate)),
        };
    }
    best.map(|(_, c)| c)
}

/// Optimal string alignment distance: insertions, deletions, substitutions and adjacent
/// transpositions each cost one.
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (n, m) = (a.len(), b.len());
    let mut d = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut v = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                v = v.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = v;
        }
    }
    d[n][m]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typos_find_their_word() {
        let words = ["prompt", "provider", "model", "agent"];
        assert_eq!(closest("promt", words), Some("prompt"));
        assert_eq!(closest("agnet", words), Some("agent"));
        assert_eq!(closest("modle", words), Some("model"));
    }

    #[test]
    fn unrelated_names_get_nothing() {
        assert_eq!(closest("zzz", ["prompt", "model"]), None);
        assert_eq!(closest("x", ["prompt"]), None);
    }

    #[test]
    fn ties_are_stable() {
        assert_eq!(closest("cat", ["cot", "bat"]), Some("bat"));
    }
}
