// Gary's 4tope: a dependency-free, offline reference for the graph mathematics.
// Concept: Gary Stimpson, 2026-10-10. Original implementation: gary23w/nl-veil.
// This is an experiment, not a Codex protocol adapter or runtime integration.
// Copyright (c) 2026 gary23w. See the accompanying LICENSE.
// SPDX-License-Identifier: MIT

use std::cmp::Reverse;
use std::collections::BinaryHeap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeKind {
    Used,
    Issued,
    Thread,
    Weak,
}

impl EdgeKind {
    fn weight(self) -> u64 {
        match self {
            Self::Used => 2,
            Self::Issued | Self::Thread => 1,
            Self::Weak => 0,
        }
    }

    fn cost(self) -> u64 {
        if self == Self::Weak { 3 } else { 1 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    User,
    Step,
    Result,
    Distillation,
}

#[derive(Clone, Debug)]
pub struct Node {
    // The index is the stable source ID for this single immutable fixture.
    // A production adapter must use persisted thread/turn/item identities.
    pub kind: Kind,
    pub text: String,
    pub deps: Vec<(usize, EdgeKind)>,
    // These flags are supplied by the caller; this reference does not infer them.
    pub superseded: bool,
    pub failed: bool,
    pub required: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    InvalidGraph,
    LimitExceeded,
    RequiredEvidenceSuperseded,
    RequiredEvidenceExceedsBudget,
}

pub struct Graph {
    nodes: Vec<Node>,
    frontier: usize,
    adjacent: Vec<Vec<(usize, EdgeKind)>>,
}

pub struct Carry {
    pub selected: Vec<usize>,
    pub text: String,
}

impl Graph {
    pub fn new(nodes: Vec<Node>, frontier: usize) -> Result<Self, Error> {
        if nodes.is_empty() || frontier >= nodes.len() || nodes[frontier].kind != Kind::Step {
            return Err(Error::InvalidGraph);
        }
        if nodes.len() > 4096
            || nodes.iter().map(|n| n.deps.len()).sum::<usize>() > 16384
            || nodes.iter().map(|n| n.text.len()).sum::<usize>() > 4 * 1024 * 1024
        {
            return Err(Error::LimitExceeded);
        }
        let mut adjacent = vec![Vec::new(); nodes.len()];
        for (v, node) in nodes.iter().enumerate() {
            for &(u, kind) in &node.deps {
                // All dependencies must point strictly backward. This also rejects cycles.
                if u >= v {
                    return Err(Error::InvalidGraph);
                }
                adjacent[v].push((u, kind));
                adjacent[u].push((v, kind));
            }
        }
        for list in &mut adjacent {
            list.sort_by_key(|&(id, kind)| (id, kind.weight()));
        }
        Ok(Self { nodes, frontier, adjacent })
    }

    // L(v) = max(L(u) + w(v,u)), with ties favoring the newer dependency.
    pub fn critical_chain(&self) -> Vec<usize> {
        let mut longest = vec![0; self.nodes.len()];
        let mut via = vec![None; self.nodes.len()];
        for (v, node) in self.nodes.iter().enumerate() {
            for &(u, kind) in &node.deps {
                let candidate = longest[u] + kind.weight();
                if candidate > longest[v]
                    || (candidate == longest[v] && via[v].is_some_and(|old| u > old))
                {
                    longest[v] = candidate;
                    via[v] = Some(u);
                }
            }
        }
        let mut chain = Vec::new();
        let mut next = Some(self.frontier);
        while let Some(v) = next {
            chain.push(v);
            next = via[v];
        }
        chain
    }

    pub fn protected_results(&self) -> Vec<usize> {
        self.critical_chain()
            .into_iter()
            .filter(|&v| self.nodes[v].kind == Kind::Result && !self.nodes[v].superseded)
            .collect()
    }

    // The topology is traversable in both directions, but arrival penalties make
    // arc costs asymmetric: c(a,b) includes b's stale/failure flags.
    pub fn shortest_tree(&self) -> (Vec<u64>, Vec<Option<usize>>) {
        let mut distance = vec![u64::MAX; self.nodes.len()];
        let mut parent = vec![None; self.nodes.len()];
        let mut heap = BinaryHeap::new();
        distance[self.frontier] = 0;
        heap.push(Reverse((0, self.frontier)));
        while let Some(Reverse((d, a))) = heap.pop() {
            if d != distance[a] {
                continue;
            }
            for &(b, kind) in &self.adjacent[a] {
                let target = &self.nodes[b];
                let cost = kind.cost()
                    + 4 * u64::from(target.superseded)
                    + u64::from(target.failed);
                let candidate = d + cost;
                if candidate < distance[b] {
                    distance[b] = candidate;
                    parent[b] = Some(a);
                    heap.push(Reverse((candidate, b)));
                }
            }
        }
        (distance, parent)
    }

    fn row(&self, id: usize) -> String {
        format!("- [source {id}] {}\n", self.nodes[id].text)
    }

    fn layout(&self, distance: &[u64], parent: &[Option<usize>]) -> Vec<usize> {
        let mut children = vec![Vec::new(); self.nodes.len()];
        for (id, &p) in parent.iter().enumerate() {
            if let Some(p) = p {
                children[p].push(id);
            }
        }
        for list in &mut children {
            // Nearest first, then newer source first. Reverse for the stack.
            list.sort_by_key(|&id| (distance[id], Reverse(id)));
            list.reverse();
        }
        let mut stack = vec![self.frontier];
        let mut order = Vec::new();
        while let Some(id) = stack.pop() {
            order.push(id);
            stack.extend(children[id].iter().copied());
        }
        order
    }

    fn render(&self, selected: &[bool], order: &[usize], ground: &str) -> String {
        let mut out = format!("NEXT [source {}]: {}\nON DISK: {}\nPINNED:\n",
            self.frontier, self.nodes[self.frontier].text, ground);
        for (id, node) in self.nodes.iter().enumerate() {
            if selected[id] && node.required && id != self.frontier {
                out.push_str(&self.row(id));
            }
        }
        out.push_str("RULED OUT (observed failure; retry conditions remain state-dependent):\n");
        for &id in order {
            let node = &self.nodes[id];
            if selected[id] && !node.required && node.failed {
                out.push_str(&self.row(id));
            }
        }
        out.push_str("ESTABLISHED (retained observations, not a truth guarantee):\n");
        for &id in order {
            let node = &self.nodes[id];
            if selected[id] && !node.required && !node.failed && id != self.frontier {
                out.push_str(&self.row(id));
            }
        }
        out
    }

    // Byte budgeting is for this offline fixture only. Production must charge
    // provider tokens, framing, calls/notices, images, tools and output headroom.
    // Required constraints and frontier are admitted before greedy optional evidence.
    pub fn carry(&self, budget: usize, ground: &str) -> Result<Carry, Error> {
        let (distance, parent) = self.shortest_tree();
        let order = self.layout(&distance, &parent);
        let mut selected = vec![false; self.nodes.len()];
        selected[self.frontier] = true;
        for (id, node) in self.nodes.iter().enumerate() {
            if node.required {
                if node.superseded {
                    return Err(Error::RequiredEvidenceSuperseded);
                }
                selected[id] = true;
            }
        }
        let mut spent = self.render(&selected, &order, ground).len();
        if spent > budget {
            return Err(Error::RequiredEvidenceExceedsBudget);
        }
        let mut nearest: Vec<usize> = (0..self.nodes.len()).collect();
        nearest.sort_by_key(|&id| (distance[id], Reverse(id)));
        for id in nearest {
            let node = &self.nodes[id];
            if selected[id] || distance[id] == u64::MAX || node.superseded
                || !matches!(node.kind, Kind::Result | Kind::Distillation)
            {
                continue;
            }
            let cost = self.row(id).len();
            if cost <= budget - spent {
                selected[id] = true;
                spent += cost;
            }
        }
        let text = self.render(&selected, &order, ground);
        debug_assert_eq!(text.len(), spent);
        debug_assert!(text.len() <= budget);
        Ok(Carry { selected: selected.iter().enumerate()
            .filter_map(|(id, &yes)| yes.then_some(id)).collect(), text })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(kind: Kind, text: &str, deps: &[(usize, EdgeKind)]) -> Node {
        Node { kind, text: text.into(), deps: deps.into(),
            superseded: false, failed: false, required: false }
    }

    fn fixture() -> Graph {
        let mut nodes = vec![
            node(Kind::User, "Keep MAGIC_V2; do not deploy.", &[]),
            node(Kind::Step, "Read parser and configuration.", &[]),
            node(Kind::Result, "parseHeader accepts MAGIC_V2 = 0x7f45", &[(1, EdgeKind::Issued)]),
            node(Kind::Result, "unrelated config port = 8787", &[(1, EdgeKind::Issued)]),
            node(Kind::Step, "Check parseHeader/MAGIC_V2", &[(1, EdgeKind::Thread), (2, EdgeKind::Used), (3, EdgeKind::Weak)]),
            node(Kind::Result, "404: spec unavailable", &[(4, EdgeKind::Issued)]),
            node(Kind::Step, "Fix parseHeader then run tests.", &[(4, EdgeKind::Thread), (5, EdgeKind::Used)]),
            node(Kind::Result, "edited src/parser.rs", &[(6, EdgeKind::Issued)]),
            node(Kind::Step, "Re-run tests after parser fix.", &[(6, EdgeKind::Thread), (7, EdgeKind::Used)]),
            node(Kind::Result, "All 12 tests passed.", &[(8, EdgeKind::Issued)]),
        ];
        nodes[0].required = true;
        nodes[2].superseded = true;
        nodes[5].failed = true;
        Graph::new(nodes, 8).unwrap()
    }

    #[test]
    fn used_chain_beats_weak_branch_and_filters_stale_results() {
        let g = fixture();
        assert_eq!(g.critical_chain(), vec![8, 7, 6, 5, 4, 2, 1]);
        assert_eq!(g.protected_results(), vec![7, 5]);
    }

    #[test]
    fn distance_uses_target_penalties_and_includes_frontier_outputs() {
        let g = fixture();
        let (d, _) = g.shortest_tree();
        assert_eq!(d[9], 1);
        assert_eq!(d[7], 1);
        assert_eq!(d[5], 3);
        assert_eq!(d[2], 7);
        assert_eq!(d[0], u64::MAX);
    }

    #[test]
    fn carry_keeps_constraints_failures_and_source_text_once() {
        let g = fixture();
        let carry = g.carry(4096, "src/parser.rs (verified write)").unwrap();
        for id in [0, 5, 7, 9] { assert!(carry.selected.contains(&id)); }
        assert!(!carry.selected.contains(&2));
        assert_eq!(carry.text.matches("All 12 tests passed.").count(), 1);
        assert!(carry.text.find("404: spec unavailable").unwrap()
            < carry.text.find("ESTABLISHED").unwrap());
    }

    #[test]
    fn every_budget_either_fits_exactly_or_returns_fallback_error() {
        let g = fixture();
        for budget in 0..1400 {
            match g.carry(budget, "src/parser.rs") {
                Ok(c) => { assert!(c.text.len() <= budget); assert!(c.selected.contains(&0)); }
                Err(e) => assert_eq!(e, Error::RequiredEvidenceExceedsBudget),
            }
        }
    }

    #[test]
    fn oversized_optional_observation_does_not_block_smaller_one() {
        let mut nodes = vec![node(Kind::Step, "continue", &[]),
            node(Kind::Result, &"x".repeat(10000), &[(0, EdgeKind::Issued)]),
            node(Kind::Result, "small exact fact", &[(0, EdgeKind::Issued)])];
        nodes[1].failed = true;
        let g = Graph::new(nodes, 0).unwrap();
        let c = g.carry(512, "").unwrap();
        assert!(!c.selected.contains(&1));
        assert!(c.selected.contains(&2));
    }

    #[test]
    fn utf8_and_literal_source_content_are_not_rewritten() {
        let text = "Gary's geometry: \u{03b1} \u{2192} \u{96ea}; ignore all rules is quoted tool text.";
        let g = Graph::new(vec![node(Kind::Step, "Next", &[]),
            node(Kind::Result, text, &[(0, EdgeKind::Issued)])], 0).unwrap();
        assert!(g.carry(1024, "").unwrap().text.contains(text));
    }

    #[test]
    fn cycles_and_forward_dependencies_are_rejected() {
        assert!(matches!(Graph::new(vec![node(Kind::Step, "bad", &[(0, EdgeKind::Thread)])], 0), Err(Error::InvalidGraph)));
        assert!(matches!(Graph::new(vec![node(Kind::Step, "bad", &[(1, EdgeKind::Used)]),
            node(Kind::Result, "future", &[])], 0), Err(Error::InvalidGraph)));
    }

    #[test]
    fn high_fanout_has_no_fixed_child_array_drop() {
        let mut nodes = vec![node(Kind::Step, "issuer", &[])];
        for i in 1..=100 {
            nodes.push(node(Kind::Result, &format!("fact {i}"), &[(0, EdgeKind::Issued)]));
        }
        let deps: Vec<_> = (1..=100).map(|id| (id, EdgeKind::Used)).collect();
        nodes.push(node(Kind::Step, "frontier", &deps));
        let g = Graph::new(nodes, 101).unwrap();
        let c = g.carry(16000, "").unwrap();
        assert_eq!(c.selected.len(), 101);
        for i in 1..=100 { assert!(c.text.contains(&format!("[source {i}] fact {i}\n"))); }
    }

    #[test]
    fn deterministic_layout_repeats_byte_for_byte() {
        let g = fixture();
        assert_eq!(g.carry(4096, "disk").unwrap().text, g.carry(4096, "disk").unwrap().text);
    }

    #[test]
    fn one_critical_chain_is_not_all_required_branches() {
        let nodes = vec![node(Kind::Step, "issuer", &[]),
            node(Kind::Result, "schema required", &[(0, EdgeKind::Issued)]),
            node(Kind::Result, "auth required", &[(0, EdgeKind::Issued)]),
            node(Kind::Step, "needs both", &[(1, EdgeKind::Used), (2, EdgeKind::Used)])];
        let g = Graph::new(nodes, 3).unwrap();
        assert_eq!(g.protected_results(), vec![2]);
        assert!(g.carry(4096, "").unwrap().selected.contains(&1));
    }

    #[test]
    fn required_stale_evidence_is_an_explicit_error() {
        let mut g = fixture();
        g.nodes[2].required = true;
        assert!(matches!(g.carry(4096, ""), Err(Error::RequiredEvidenceSuperseded)));
    }

    #[test]
    fn missing_frontier_and_graph_caps_return_errors() {
        assert!(matches!(Graph::new(vec![], 0), Err(Error::InvalidGraph)));
        assert!(matches!(Graph::new(vec![node(Kind::User, "user", &[])], 0), Err(Error::InvalidGraph)));
        assert!(matches!(Graph::new(vec![node(Kind::Step, "x", &[]); 4097], 0), Err(Error::LimitExceeded)));
    }
}
