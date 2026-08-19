//! Strongly connected components over a dense node index.
//!
//! A caller gives a node count and a successor list per node. The
//! walk returns the components and the component index of each node.
//!
//! The emission order is part of the contract, because a caller uses
//! it as a processing schedule:
//!
//! - Roots run in ascending node index.
//! - Successors run in ascending position in the successor list.
//! - Components emit callees-first, so every component a component
//!   references emits before it.
//!
//! `lm-bytecode` reads that order as the definition hash schedule, so
//! a change to it moves every artifact hash. Treat the order as
//! pinned.
//!
//! The walk keeps its own work stack. A caller may pass untrusted
//! input, so the traversal must never grow the host stack.

/// The marker for a node the walk has not reached.
const UNSET: u32 = u32::MAX;

/// The components of one directed graph, and the component index of
/// each node.
///
/// `succ[n]` holds the successors of node `n`. Every successor must
/// be less than `node_count`.
///
/// A node with no cycle forms a component of one member. A node
/// reaches its own component index through the second return value.
pub fn components(node_count: usize, succ: &[Vec<u32>]) -> (Vec<Vec<u32>>, Vec<u32>) {
    let n = node_count;
    let mut index = vec![UNSET; n];
    let mut low = vec![0u32; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<u32> = Vec::new();
    let mut next = 0u32;
    let mut comps: Vec<Vec<u32>> = Vec::new();
    let mut comp_of = vec![UNSET; n];
    // The explicit DFS work stack: (node, next successor position).
    let mut work: Vec<(u32, usize)> = Vec::new();
    for root in 0..n as u32 {
        if index[root as usize] != UNSET {
            continue;
        }
        work.push((root, 0));
        index[root as usize] = next;
        low[root as usize] = next;
        next += 1;
        stack.push(root);
        on_stack[root as usize] = true;
        while let Some((node, pos)) = work.last().copied() {
            let succs = &succ[node as usize];
            if pos < succs.len() {
                work.last_mut().expect("frame").1 += 1;
                let child = succs[pos];
                if index[child as usize] == UNSET {
                    index[child as usize] = next;
                    low[child as usize] = next;
                    next += 1;
                    stack.push(child);
                    on_stack[child as usize] = true;
                    work.push((child, 0));
                } else if on_stack[child as usize] {
                    let li = low[node as usize].min(index[child as usize]);
                    low[node as usize] = li;
                }
            } else {
                work.pop();
                if let Some((parent, _)) = work.last() {
                    let li = low[*parent as usize].min(low[node as usize]);
                    low[*parent as usize] = li;
                }
                if low[node as usize] == index[node as usize] {
                    let mut comp = Vec::new();
                    loop {
                        let member = stack.pop().expect("component stack");
                        on_stack[member as usize] = false;
                        comp_of[member as usize] = comps.len() as u32;
                        comp.push(member);
                        if member == node {
                            break;
                        }
                    }
                    comps.push(comp);
                }
            }
        }
    }
    (comps, comp_of)
}

#[cfg(test)]
mod tests {
    use super::components;

    #[test]
    fn a_node_without_an_edge_forms_its_own_component() {
        let (comps, comp_of) = components(3, &[vec![], vec![], vec![]]);
        assert_eq!(comps.len(), 3);
        assert_eq!(comp_of, vec![0, 1, 2]);
    }

    #[test]
    fn a_cycle_forms_one_component() {
        // 0 -> 1 -> 2 -> 0
        let (comps, comp_of) = components(3, &[vec![1], vec![2], vec![0]]);
        assert_eq!(comps.len(), 1);
        assert_eq!(comps[0].len(), 3);
        assert_eq!(comp_of, vec![0, 0, 0]);
    }

    #[test]
    fn components_emit_callees_first() {
        // 0 -> 1, and 1 has no successor, so 1 emits before 0.
        let (comps, comp_of) = components(2, &[vec![1], vec![]]);
        assert_eq!(comps, vec![vec![1], vec![0]]);
        assert_eq!(comp_of, vec![1, 0]);
    }

    #[test]
    fn a_referenced_cycle_emits_before_its_referrer() {
        // 0 -> 1, 1 -> 2, 2 -> 1. The cycle {1,2} emits first.
        let (comps, comp_of) = components(3, &[vec![1], vec![2], vec![1]]);
        assert_eq!(comps.len(), 2);
        assert_eq!(comps[0].len(), 2);
        assert_eq!(comps[1], vec![0]);
        assert_eq!(comp_of[0], 1);
        assert_eq!(comp_of[1], comp_of[2]);
    }

    #[test]
    fn a_deep_chain_does_not_grow_the_host_stack() {
        // The walk keeps its own stack, so a long chain is ordinary
        // input and not a crash.
        let n = 200_000;
        let succ: Vec<Vec<u32>> = (0..n)
            .map(|i| {
                if i + 1 < n {
                    vec![i as u32 + 1]
                } else {
                    vec![]
                }
            })
            .collect();
        let (comps, _) = components(n, &succ);
        assert_eq!(comps.len(), n);
        // The last node has no successor, so it emits first.
        assert_eq!(comps[0], vec![n as u32 - 1]);
    }

    #[test]
    fn a_self_edge_is_one_component() {
        let (comps, comp_of) = components(1, &[vec![0]]);
        assert_eq!(comps, vec![vec![0]]);
        assert_eq!(comp_of, vec![0]);
    }
}
