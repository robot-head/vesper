//! Concrete clock-guarded dependency graph checking (manuscript §5.4).
//!
//! This checks a caller-supplied graph; it does not extract dependencies from
//! Vesper programs, typecheck clock binders, or prove semantic productivity.

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct ClockId(pub u32);

/// A dependency from `from` to `to`, optionally delayed by one named clock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Edge {
    pub from: usize,
    pub to: usize,
    pub delayed_by: Option<ClockId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub max_nodes: usize,
    pub max_edges: usize,
    pub max_work: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    NodeLimit {
        count: usize,
        max: usize,
    },
    EdgeLimit {
        count: usize,
        max: usize,
    },
    WorkLimit {
        max: usize,
    },
    InvalidEndpoint {
        edge: usize,
        node: usize,
    },
    AllocationFailed,
    /// This back edge witnesses a cycle after edges guarded by the clock are removed.
    UnguardedCycle {
        edge: usize,
        from: usize,
        to: usize,
    },
}

/// Reject a cycle that does not pass through an edge delayed by `clock`.
///
/// All components are checked, including those unreachable from other nodes.
/// A delay on a different clock is not a guard for this query. Work is prepaid
/// for the iterative traversal and endpoint validation before allocation.
pub fn check_guarded(
    node_count: usize,
    edges: &[Edge],
    clock: ClockId,
    limits: Limits,
) -> Result<(), Error> {
    if node_count > limits.max_nodes {
        return Err(Error::NodeLimit {
            count: node_count,
            max: limits.max_nodes,
        });
    }
    if edges.len() > limits.max_edges {
        return Err(Error::EdgeLimit {
            count: edges.len(),
            max: limits.max_edges,
        });
    }
    let work = node_count
        .checked_mul(edges.len())
        .and_then(|n| {
            node_count
                .checked_mul(4)
                .and_then(|nodes| n.checked_add(nodes))
        })
        .and_then(|n| {
            edges
                .len()
                .checked_mul(2)
                .and_then(|edges| n.checked_add(edges))
        });
    if work.is_none_or(|n| n > limits.max_work) {
        return Err(Error::WorkLimit {
            max: limits.max_work,
        });
    }
    for (index, edge) in edges.iter().enumerate() {
        for node in [edge.from, edge.to] {
            if node >= node_count {
                return Err(Error::InvalidEndpoint { edge: index, node });
            }
        }
    }
    // 0 = unseen, 1 = on DFS path, 2 = complete. Each stack frame scans
    // the caller's edge slice once; there is no recursion or adjacency allocation.
    let mut color = Vec::new();
    color
        .try_reserve_exact(node_count)
        .map_err(|_| Error::AllocationFailed)?;
    color.resize(node_count, 0u8);
    let mut stack: Vec<(usize, usize)> = Vec::new();
    stack
        .try_reserve_exact(node_count)
        .map_err(|_| Error::AllocationFailed)?;
    for root in 0..node_count {
        if color[root] != 0 {
            continue;
        }
        color[root] = 1;
        stack.push((root, 0));
        while let Some((node, next)) = stack.last_mut() {
            if *next == edges.len() {
                color[*node] = 2;
                stack.pop();
                continue;
            }
            let index = *next;
            *next += 1;
            let edge = edges[index];
            if edge.from != *node || edge.delayed_by == Some(clock) {
                continue;
            }
            match color[edge.to] {
                0 => {
                    color[edge.to] = 1;
                    stack.push((edge.to, 0));
                }
                1 => {
                    return Err(Error::UnguardedCycle {
                        edge: index,
                        from: edge.from,
                        to: edge.to,
                    });
                }
                _ => {}
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_independent_transitive_closure_for_all_three_node_graphs() {
        let clock = ClockId(1);
        let limits = Limits {
            max_nodes: 3,
            max_edges: 9,
            max_work: 100,
        };
        for graph in 0u32..3u32.pow(9) {
            let mut digits = graph;
            let mut edges = Vec::new();
            let mut reach = [[false; 3]; 3];
            for i in 0..9 {
                let state = digits % 3;
                digits /= 3;
                if state != 0 {
                    let from = i / 3;
                    let to = i % 3;
                    let delayed_by = if state == 1 {
                        Some(clock)
                    } else if i % 2 == 0 {
                        Some(ClockId(2))
                    } else {
                        None
                    };
                    edges.push(Edge {
                        from,
                        to,
                        delayed_by,
                    });
                    if delayed_by != Some(clock) {
                        reach[from][to] = true;
                    }
                }
            }
            for k in 0..3 {
                for i in 0..3 {
                    for j in 0..3 {
                        reach[i][j] |= reach[i][k] && reach[k][j];
                    }
                }
            }
            let has_cycle = (0..3).any(|i| reach[i][i]);
            assert_eq!(
                check_guarded(3, &edges, clock, limits).is_err(),
                has_cycle,
                "graph {graph}"
            );
        }
    }

    #[test]
    fn limits_and_clock_identity_are_enforced() {
        let edge = Edge {
            from: 0,
            to: 0,
            delayed_by: Some(ClockId(1)),
        };
        let limits = Limits {
            max_nodes: 1,
            max_edges: 1,
            max_work: 7,
        };
        assert_eq!(check_guarded(1, &[edge], ClockId(1), limits), Ok(()));
        assert_eq!(
            check_guarded(1, &[edge], ClockId(2), limits),
            Err(Error::UnguardedCycle {
                edge: 0,
                from: 0,
                to: 0
            })
        );
        assert_eq!(
            check_guarded(1, &[Edge { to: 1, ..edge }], ClockId(1), limits),
            Err(Error::InvalidEndpoint { edge: 0, node: 1 })
        );
        assert_eq!(
            check_guarded(2, &[], ClockId(1), limits),
            Err(Error::NodeLimit { count: 2, max: 1 })
        );
        assert_eq!(
            check_guarded(1, &[edge, edge], ClockId(1), limits),
            Err(Error::EdgeLimit { count: 2, max: 1 })
        );
        assert_eq!(
            check_guarded(
                1,
                &[edge],
                ClockId(1),
                Limits {
                    max_work: 6,
                    ..limits
                }
            ),
            Err(Error::WorkLimit { max: 6 })
        );
        assert_eq!(
            check_guarded(
                0,
                &[],
                ClockId(1),
                Limits {
                    max_nodes: 0,
                    max_edges: 0,
                    max_work: 0
                }
            ),
            Ok(())
        );
    }
}
