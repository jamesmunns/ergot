use std::collections::{HashMap, HashSet};

pub struct GraphMap<N: GraphNode> {
    index: usize,
    nodes: HashMap<usize, N>,
    edges: HashSet<(usize, usize)>,
}

impl<N: GraphNode> GraphMap<N> {
    pub fn new() -> Self {
        Self {
            index: 0,
            nodes: HashMap::new(),
            edges: HashSet::new(),
        }
    }

    pub fn add_node(&mut self, n: N) -> usize {
        let i = self.index;
        self.index += 1;
        self.nodes.insert(i, n);
        i
    }

    pub fn add_edge(&mut self, lhs: usize, rhs: usize) {
        let (a, b) = sort_args(lhs, rhs);
        if let Some(mut a_node) = self.nodes.remove(&a) {
            if let Some(b_node) = self.nodes.get(&b) {
                a_node.edge_added(b_node, b);
                self.edges.insert((a, b));
            }
            self.nodes.insert(a, a_node);
        }
    }

    pub fn remove_edge(&mut self, lhs: usize, rhs: usize) {
        let (a, b) = sort_args(lhs, rhs);
        if self.edges.remove(&(lhs, rhs)) {
            if let Some(mut a_node) = self.nodes.remove(&a) {
                if let Some(b_node) = self.nodes.get(&b) {
                    a_node.edge_removed(b_node, b);
                }
                self.nodes.insert(a, a_node);
            }
        }
    }

    pub fn dot_graph(&self) -> String {
        itertools::Itertools::intersperse(
            ["graph G {".to_string()]
                .into_iter()
                .chain(self.nodes.keys().map(|x| x.to_string()))
                .chain(self.edges.iter().map(|(a, b)| format!("{a} -- {b}")))
                .chain(["}".to_string()]),
            " ".to_string(),
        )
        .collect()
    }
}

fn sort_args(lhs: usize, rhs: usize) -> (usize, usize) {
    if lhs > rhs { (lhs, rhs) } else { (rhs, lhs) }
}

impl<N: GraphNode> Default for GraphMap<N> {
    fn default() -> Self {
        Self::new()
    }
}

pub trait GraphNode {
    fn edge_added(&mut self, other: &Self, other_id: usize);
    fn edge_removed(&mut self, other: &Self, other_id: usize);
}
