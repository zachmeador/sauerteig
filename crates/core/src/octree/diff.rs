//! What a write changed: the stored leaves it removed and added, and where
//! solid geometry or surface materials changed. Unchanged subtrees compare as
//! equal pointers, so the walk costs what the write changed.

use super::{split, Leaf, LeafGeometry, Node, NodeBox, Octree};
use crate::{ChunkKey, FineBox};
use std::collections::BTreeSet;
use std::sync::Arc;

/// Leaves with solid geometry that a write removed or added, and the boxes
/// whose solid geometry or surface materials changed. A leaf whose cube
/// material alone changed is in none of them.
#[derive(Default)]
pub(crate) struct Changes {
    pub removed: Vec<(NodeBox, Leaf)>,
    pub added: Vec<(NodeBox, Leaf)>,
    pub changed: Vec<FineBox>,
}

fn same(a: &Node, b: &Node) -> bool {
    match (a, b) {
        (Node::Branch(x), Node::Branch(y)) => Arc::ptr_eq(x, y),
        (Node::Branch(_), _) | (_, Node::Branch(_)) => false,
        _ => a == b,
    }
}

/// A leaf's solid geometry and surface materials, without its cube material.
fn surface(node: &Node) -> Option<(LeafGeometry, [u8; 6])> {
    node.leaf()
        .filter(|leaf| leaf.geometry != LeafGeometry::Empty)
        .map(|leaf| (leaf.geometry, leaf.faces))
}

fn stored(node: &Node) -> Leaf {
    node.leaf().expect("a leaf")
}

/// Compares two nodes at the same place. A stored node is `real`; the parts a
/// stored leaf splits into, to be compared with a branch, are not.
fn walk(a: &Node, real_a: bool, b: &Node, real_b: bool, at: NodeBox, out: &mut Changes) {
    if real_a && real_b && same(a, b) {
        return;
    }
    match (a, b) {
        (Node::Branch(x), Node::Branch(y)) => {
            for i in 0..8 {
                walk(&x[i], real_a, &y[i], real_b, at.child(i), out);
            }
        }
        (Node::Branch(x), leaf) => {
            if real_b && leaf.has_geometry() {
                out.added.push((at, stored(leaf)));
            }
            let parts = split(leaf, at);
            for i in 0..8 {
                walk(&x[i], real_a, &parts[i], false, at.child(i), out);
            }
        }
        (leaf, Node::Branch(y)) => {
            if real_a && leaf.has_geometry() {
                out.removed.push((at, stored(leaf)));
            }
            let parts = split(leaf, at);
            for i in 0..8 {
                walk(&parts[i], false, &y[i], real_b, at.child(i), out);
            }
        }
        (x, y) => {
            let unchanged = surface(x) == surface(y);
            let both = real_a && real_b;
            if real_a && x.has_geometry() && !(both && unchanged) {
                out.removed.push((at, stored(x)));
            }
            if real_b && y.has_geometry() && !(both && unchanged) {
                out.added.push((at, stored(y)));
            }
            if !unchanged {
                out.changed.push(at.fine_box());
            }
        }
    }
}

impl Octree {
    /// What changed from an earlier version of this tree.
    pub(crate) fn changes_from(&self, before: &Octree) -> Changes {
        let mut out = Changes::default();
        if before.root_box() != self.root_box() {
            // A world's root never moves; a different one changes everything.
            for (tree, list) in [(before, &mut out.removed), (self, &mut out.added)] {
                tree.for_each_leaf(|at, leaf| {
                    if leaf.geometry != LeafGeometry::Empty {
                        list.push((at, leaf));
                    }
                });
            }
            out.changed = out
                .removed
                .iter()
                .chain(&out.added)
                .map(|(at, _)| at.fine_box())
                .collect();
            return out;
        }
        walk(
            before.root_node(),
            true,
            self.root_node(),
            true,
            self.root_box(),
            &mut out,
        );
        out
    }

    /// The mesh units to build again after a change: those owning a removed or
    /// added leaf, and those owning a leaf beside changed geometry, whose
    /// faces may now be covered or uncovered. Coverage by a shaped leaf or a
    /// piece depends on its kind and box, not only its surface, so a leaf
    /// beside one that was split or merged is built again too. Full leaves
    /// cover whatever their size, and empty ones never cover.
    pub(crate) fn dirty_units(&self, changes: &Changes) -> Vec<ChunkKey> {
        let mut keys = BTreeSet::new();
        let mut beside = changes.changed.clone();
        for (at, leaf) in changes.removed.iter().chain(&changes.added) {
            keys.insert(self.unit_key(at.min));
            if matches!(
                leaf.geometry,
                LeafGeometry::Shaped(_) | LeafGeometry::Piece(_)
            ) {
                beside.push(at.fine_box());
            }
        }
        for b in &beside {
            let grown = FineBox {
                min: b.min.map(|v| v - 1),
                max: b.max.map(|v| v + 1),
            };
            self.visit_box(grown, &mut |at, leaf| {
                if leaf.has_geometry() {
                    keys.insert(self.unit_key(at.min));
                }
                true
            });
        }
        keys.into_iter().collect()
    }
}
