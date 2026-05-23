use serde::{Deserialize, Serialize};

/// Recursive layout tree (mirrors frontend types).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum LayoutNode {
    #[default]
    Empty,
    Leaf {
        #[serde(rename = "agentId")]
        agent_id: String,
    },
    Split {
        direction: SplitDir,
        ratio: f32,
        a: Box<LayoutNode>,
        b: Box<LayoutNode>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SplitDir {
    H,
    V,
}

impl LayoutNode {
    /// Return all leaf agent IDs in left-to-right order.
    pub fn leaves(&self) -> Vec<String> {
        let mut out = Vec::new();
        self.collect_leaves(&mut out);
        out
    }

    fn collect_leaves(&self, out: &mut Vec<String>) {
        match self {
            LayoutNode::Empty => {}
            LayoutNode::Leaf { agent_id } => out.push(agent_id.clone()),
            LayoutNode::Split { a, b, .. } => {
                a.collect_leaves(out);
                b.collect_leaves(out);
            }
        }
    }

    /// Insert new leaf by repeatedly splitting the current rightmost leaf,
    /// alternating H/V to produce a balanced grid. Used by `spawn_agent count=N`.
    pub fn insert_grid(self, agent_id: &str, depth: usize) -> Self {
        match self {
            LayoutNode::Empty => LayoutNode::Leaf {
                agent_id: agent_id.to_string(),
            },
            LayoutNode::Leaf { .. } => LayoutNode::Split {
                direction: if depth.is_multiple_of(2) {
                    SplitDir::V
                } else {
                    SplitDir::H
                },
                ratio: 0.5,
                a: Box::new(self),
                b: Box::new(LayoutNode::Leaf {
                    agent_id: agent_id.to_string(),
                }),
            },
            LayoutNode::Split {
                direction,
                ratio,
                a,
                b,
            } => {
                // Recurse into the deeper subtree to keep tree balanced.
                let a_n = a.leaf_count();
                let b_n = b.leaf_count();
                if a_n <= b_n {
                    LayoutNode::Split {
                        direction,
                        ratio,
                        a: Box::new(a.insert_grid(agent_id, depth + 1)),
                        b,
                    }
                } else {
                    LayoutNode::Split {
                        direction,
                        ratio,
                        a,
                        b: Box::new(b.insert_grid(agent_id, depth + 1)),
                    }
                }
            }
        }
    }

    pub fn leaf_count(&self) -> usize {
        match self {
            LayoutNode::Empty => 0,
            LayoutNode::Leaf { .. } => 1,
            LayoutNode::Split { a, b, .. } => a.leaf_count() + b.leaf_count(),
        }
    }

    /// Remove a leaf by agent id; collapse splits. Returns true if removed.
    pub fn remove_leaf(self, agent_id: &str) -> (Self, bool) {
        match self {
            LayoutNode::Empty => (LayoutNode::Empty, false),
            LayoutNode::Leaf { agent_id: id } if id == agent_id => (LayoutNode::Empty, true),
            LayoutNode::Leaf { agent_id: id } => (LayoutNode::Leaf { agent_id: id }, false),
            LayoutNode::Split {
                direction,
                ratio,
                a,
                b,
            } => {
                let (a2, removed_a) = a.remove_leaf(agent_id);
                if removed_a {
                    if matches!(a2, LayoutNode::Empty) {
                        return (*b, true);
                    }
                    return (
                        LayoutNode::Split {
                            direction,
                            ratio,
                            a: Box::new(a2),
                            b,
                        },
                        true,
                    );
                }
                let (b2, removed_b) = b.remove_leaf(agent_id);
                if removed_b {
                    if matches!(b2, LayoutNode::Empty) {
                        return (a2, true);
                    }
                    return (
                        LayoutNode::Split {
                            direction,
                            ratio,
                            a: Box::new(a2),
                            b: Box::new(b2),
                        },
                        true,
                    );
                }
                (
                    LayoutNode::Split {
                        direction,
                        ratio,
                        a: Box::new(a2),
                        b: Box::new(b2),
                    },
                    false,
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_grid_grows_balanced() {
        let mut tree = LayoutNode::Empty;
        for i in 0..4 {
            tree = tree.insert_grid(&format!("a{}", i), 0);
        }
        assert_eq!(tree.leaf_count(), 4);
        assert_eq!(tree.leaves().len(), 4);
    }

    #[test]
    fn remove_collapses_split() {
        let mut tree = LayoutNode::Empty;
        for i in 0..3 {
            tree = tree.insert_grid(&format!("a{}", i), 0);
        }
        let (tree, ok) = tree.remove_leaf("a1");
        assert!(ok);
        assert_eq!(tree.leaf_count(), 2);
    }
}
