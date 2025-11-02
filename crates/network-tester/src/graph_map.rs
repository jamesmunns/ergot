use std::collections::{HashMap, HashSet};

pub struct GraphMap<M, N: GraphNode<M>> {
    index: usize,
    nodes: HashMap<usize, N>,
    edges: HashMap<(usize, usize), M>,
}

impl<M, N: GraphNode<M>> GraphMap<M, N> {
    pub fn new() -> Self {
        Self {
            index: 0,
            nodes: HashMap::new(),
            edges: HashMap::new(),
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
            if let Some(b_node) = self.nodes.remove(&b) {
                let result = a_node.edge_added(&b_node, b);
                self.edges.insert((a, b), result);
                self.nodes.insert(b, b_node);
            }
            self.nodes.insert(a, a_node);
        }
    }

    pub fn remove_edge(&mut self, lhs: usize, rhs: usize) {
        let (a, b) = sort_args(lhs, rhs);
        self.edges.remove(&(a, b));
    }

    pub fn dot_graph(&self) -> String {
        itertools::Itertools::intersperse(
            ["graph G {".to_string()]
                .into_iter()
                .chain(self.nodes.keys().map(|x| x.to_string()))
                .chain(self.edges.keys().map(|(a, b)| format!("{a} -- {b}")))
                .chain(["}".to_string()]),
            " ".to_string(),
        )
        .collect()
    }
}

fn sort_args(lhs: usize, rhs: usize) -> (usize, usize) {
    if lhs > rhs { (lhs, rhs) } else { (rhs, lhs) }
}

impl<M, N: GraphNode<M>> Default for GraphMap<M, N> {
    fn default() -> Self {
        Self::new()
    }
}

pub trait GraphNode<M> {
    fn edge_added(&mut self, other: &Self, other_id: usize) -> M;
}
