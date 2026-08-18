//! Pattern usefulness analysis.
//!
//! This is the standard specialized-matrix usefulness algorithm.
//! Exhaustiveness of a `case` is the non-usefulness of a wildcard
//! after all arms. An arm is unreachable when it is not useful after
//! the arms before it. A work budget bounds pathological inputs, so
//! the checker never runs an unbounded search.

/// One abstract pattern for the analysis. Bindings and wildcards are
/// both `Wild`.
#[derive(Debug, Clone, PartialEq)]
pub enum APat {
    Wild,
    Int(i64),
    Bool(bool),
    Str(String),
    /// A final case class with sub-patterns for its fields.
    Ctor(u32, Vec<APat>),
    /// A tuple with one sub-pattern for each element. A tuple type has
    /// one constructor, so a tuple head is always a complete
    /// signature.
    Tuple(Vec<APat>),
}

/// Class metadata for the analysis.
pub trait PatMeta {
    /// The field count of one case class.
    fn arm_arity(&self, class: u32) -> usize;
    /// Every case class of the family that contains `class`, in
    /// declaration order.
    fn family_arms(&self, class: u32) -> Vec<u32>;
}

/// The analysis exceeded its work budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetExceeded;

/// Return true when the row `v` matches at least one value that no
/// row of `matrix` matches.
pub fn useful(
    meta: &impl PatMeta,
    matrix: &[Vec<APat>],
    v: &[APat],
    budget: &mut u64,
) -> Result<bool, BudgetExceeded> {
    if *budget == 0 {
        return Err(BudgetExceeded);
    }
    *budget -= 1;
    if v.is_empty() {
        return Ok(matrix.is_empty());
    }
    match &v[0] {
        APat::Ctor(class, args) => {
            let arity = args.len();
            let spec = specialize_matrix(matrix, &Head::Class(*class), arity);
            let mut sv: Vec<APat> = args.clone();
            sv.extend_from_slice(&v[1..]);
            useful(meta, &spec, &sv, budget)
        }
        APat::Tuple(args) => {
            let arity = args.len();
            let spec = specialize_matrix(matrix, &Head::Tuple(arity), arity);
            let mut sv: Vec<APat> = args.clone();
            sv.extend_from_slice(&v[1..]);
            useful(meta, &spec, &sv, budget)
        }
        APat::Int(value) => {
            let spec = specialize_matrix(matrix, &Head::Int(*value), 0);
            useful(meta, &spec, &v[1..], budget)
        }
        APat::Bool(value) => {
            let spec = specialize_matrix(matrix, &Head::Bool(*value), 0);
            useful(meta, &spec, &v[1..], budget)
        }
        APat::Str(value) => {
            let spec = specialize_matrix(matrix, &Head::Str(value.clone()), 0);
            useful(meta, &spec, &v[1..], budget)
        }
        APat::Wild => {
            let heads = column_heads(matrix);
            if complete_signature(meta, &heads) {
                for head in &heads {
                    let arity = match head {
                        Head::Class(c) => meta.arm_arity(*c),
                        Head::Tuple(n) => *n,
                        _ => 0,
                    };
                    let spec = specialize_matrix(matrix, head, arity);
                    let mut sv: Vec<APat> = vec![APat::Wild; arity];
                    sv.extend_from_slice(&v[1..]);
                    if useful(meta, &spec, &sv, budget)? {
                        return Ok(true);
                    }
                }
                // With a complete Bool signature both literals were
                // covered above. With a complete class signature every
                // arm was covered. The wildcard adds nothing more.
                Ok(false)
            } else {
                let defaulted = default_matrix(matrix);
                useful(meta, &defaulted, &v[1..], budget)
            }
        }
    }
}

/// The head constructor of one pattern column entry.
#[derive(Debug, Clone, PartialEq)]
enum Head {
    Class(u32),
    Tuple(usize),
    Int(i64),
    Bool(bool),
    Str(String),
}

fn pattern_head(pat: &APat) -> Option<Head> {
    match pat {
        APat::Wild => None,
        APat::Int(v) => Some(Head::Int(*v)),
        APat::Bool(v) => Some(Head::Bool(*v)),
        APat::Str(v) => Some(Head::Str(v.clone())),
        APat::Ctor(c, _) => Some(Head::Class(*c)),
        APat::Tuple(args) => Some(Head::Tuple(args.len())),
    }
}

/// Collect the distinct head constructors in the first column.
fn column_heads(matrix: &[Vec<APat>]) -> Vec<Head> {
    let mut heads: Vec<Head> = Vec::new();
    for row in matrix {
        if let Some(head) = row.first().and_then(pattern_head) {
            if !heads.contains(&head) {
                heads.push(head);
            }
        }
    }
    heads
}

/// Return true when the observed heads form a complete signature for
/// their type. Integer and string literals are never complete.
fn complete_signature(meta: &impl PatMeta, heads: &[Head]) -> bool {
    let Some(first) = heads.first() else {
        return false;
    };
    match first {
        Head::Bool(_) => heads.contains(&Head::Bool(true)) && heads.contains(&Head::Bool(false)),
        Head::Class(c) => {
            let arms = meta.family_arms(*c);
            arms.iter().all(|arm| heads.contains(&Head::Class(*arm)))
        }
        // A tuple type has one constructor, so one tuple head
        // covers it.
        Head::Tuple(_) => true,
        Head::Int(_) | Head::Str(_) => false,
    }
}

fn specialize_row(row: &[APat], head: &Head, arity: usize) -> Option<Vec<APat>> {
    let first = row.first().expect("rows keep one column per position");
    let expanded: Option<Vec<APat>> = match (first, head) {
        (APat::Wild, _) => Some(vec![APat::Wild; arity]),
        (APat::Ctor(c, args), Head::Class(h)) if c == h => Some(args.clone()),
        (APat::Tuple(args), Head::Tuple(n)) if args.len() == *n => Some(args.clone()),
        (APat::Int(v), Head::Int(h)) if v == h => Some(Vec::new()),
        (APat::Bool(v), Head::Bool(h)) if v == h => Some(Vec::new()),
        (APat::Str(v), Head::Str(h)) if v == h => Some(Vec::new()),
        _ => None,
    };
    expanded.map(|mut prefix| {
        prefix.extend_from_slice(&row[1..]);
        prefix
    })
}

fn specialize_matrix(matrix: &[Vec<APat>], head: &Head, arity: usize) -> Vec<Vec<APat>> {
    matrix
        .iter()
        .filter_map(|row| specialize_row(row, head, arity))
        .collect()
}

fn default_matrix(matrix: &[Vec<APat>]) -> Vec<Vec<APat>> {
    matrix
        .iter()
        .filter_map(|row| match row.first() {
            Some(APat::Wild) => Some(row[1..].to_vec()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny family: classes 1 and 2 are the arms; class 1 has one
    /// field, class 2 has none.
    struct TestMeta;

    impl PatMeta for TestMeta {
        fn arm_arity(&self, class: u32) -> usize {
            if class == 1 {
                1
            } else {
                0
            }
        }

        fn family_arms(&self, _class: u32) -> Vec<u32> {
            vec![1, 2]
        }
    }

    fn budget() -> u64 {
        1_000_000
    }

    #[test]
    fn bool_true_false_is_exhaustive() {
        let matrix = vec![vec![APat::Bool(true)], vec![APat::Bool(false)]];
        let mut b = budget();
        assert!(!useful(&TestMeta, &matrix, &[APat::Wild], &mut b).unwrap());
    }

    #[test]
    fn missing_arm_is_detected() {
        let matrix = vec![vec![APat::Ctor(1, vec![APat::Wild])]];
        let mut b = budget();
        assert!(useful(&TestMeta, &matrix, &[APat::Wild], &mut b).unwrap());
    }

    #[test]
    fn nested_coverage_is_exhaustive() {
        // Some(Some(_)) | Some(None) | None over Option[Option[T]],
        // with class 1 = Some, class 2 = None.
        let matrix = vec![
            vec![APat::Ctor(1, vec![APat::Ctor(1, vec![APat::Wild])])],
            vec![APat::Ctor(1, vec![APat::Ctor(2, vec![])])],
            vec![APat::Ctor(2, vec![])],
        ];
        let mut b = budget();
        assert!(!useful(&TestMeta, &matrix, &[APat::Wild], &mut b).unwrap());
    }

    #[test]
    fn duplicate_arm_is_not_useful() {
        let matrix = vec![vec![APat::Ctor(2, vec![])]];
        let mut b = budget();
        assert!(!useful(&TestMeta, &matrix, &[APat::Ctor(2, vec![])], &mut b).unwrap());
    }

    #[test]
    fn integer_literals_are_never_complete() {
        let matrix = vec![vec![APat::Int(1)], vec![APat::Int(2)]];
        let mut b = budget();
        assert!(useful(&TestMeta, &matrix, &[APat::Wild], &mut b).unwrap());
    }

    #[test]
    fn budget_exhaustion_is_reported() {
        let matrix = vec![vec![APat::Ctor(1, vec![APat::Wild])]];
        let mut b = 1;
        assert_eq!(
            useful(&TestMeta, &matrix, &[APat::Wild], &mut b),
            Err(BudgetExceeded)
        );
    }
}
