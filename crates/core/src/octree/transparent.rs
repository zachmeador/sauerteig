//! Water and glass surfaces, one mesh per mesh unit.
//!
//! A water or glass surface lies where a leaf's cube material meets something
//! else: air, the other material, or solid geometry that does not cover it.
//! Faces fully against solid geometry, and faces between two cubes of one
//! material, are left out, so a lake costs its surface and a glass wall costs
//! its two sides. A water-glass interface keeps both faces. Leaves that are
//! solid all through draw nothing: water inside rock is never seen.
//!
//! Each leaf's faces belong to the unit holding the leaf's minimum corner,
//! as opaque faces do. Within a unit, coplanar faces of one material merge
//! into rectangles. Every rectangle carries flags for its edges where the
//! surface really ends, so glass's edge band follows the surface and not the
//! seams of the merge. An edge is flagged unless faces of the same surface,
//! in this unit or any other, continue along all of it, whether in view or
//! running on under solid geometry: a pane set into a wall has no band where
//! it enters the wall.

use super::mesh::{collect_owned, faces_hold, faces_on, Face, Region, Q};
use super::{volume_material_of, LeafGeometry, Node, NodeBox, Octree};
use crate::{ChunkKey, FineBox, VolumeMaterial};
use std::collections::BTreeMap;

/// Edge flags of a quad, over its in-plane axes `u = (axis + 1) % 3` and
/// `v = (axis + 2) % 3`: set where the surface ends along that edge.
pub const EDGE_U_MIN: u8 = 1;
pub const EDGE_V_MIN: u8 = 2;
pub const EDGE_U_MAX: u8 = 4;
pub const EDGE_V_MAX: u8 = 8;

/// A corner of a water or glass quad.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransparentVertex {
    pub position: [f32; 3],
    /// The outward normal of the quad.
    pub normal: [f32; 3],
    /// The corner's place in the quad along `u` and `v`, in metres.
    pub uv: [f32; 2],
    /// The quad's extent along `u` and `v`, in metres.
    pub size: [f32; 2],
    /// 0 for water and 1 for glass.
    pub material: u8,
    /// `EDGE_*` flags.
    pub edges: u8,
}

/// The water and glass surfaces of one mesh unit: four vertices to a quad,
/// counter-clockwise seen from outside, drawn as the triangles (0, 1, 2) and
/// (0, 2, 3) of each.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TransparentMesh {
    pub vertices: Vec<TransparentVertex>,
}

impl TransparentMesh {
    pub fn quads(&self) -> usize {
        self.vertices.len() / 4
    }
}

/// A rectangle on a face plane, in eighths of a metre along `u` and `v`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct R {
    min: [i32; 2],
    max: [i32; 2],
}

/// Which surface a rectangle belongs to: its axis, its side, its plane in
/// eighths and its material (0 water, 1 glass).
type Group = (usize, bool, i32, u8);

fn material_index(material: VolumeMaterial) -> u8 {
    match material {
        VolumeMaterial::Water => 0,
        VolumeMaterial::Glass => 1,
    }
}

/// The medium a leaf draws: water or glass in a leaf that is not solid all
/// through.
fn drawn(node: &Node) -> Option<VolumeMaterial> {
    if matches!(node, Node::Full(..) | Node::Branch(_)) {
        return None;
    }
    volume_material_of(node.cube())
}

/// Merges rectangles that share a whole edge, along each axis in turn until
/// none do. The rectangles are disjoint.
fn merge(mut rects: Vec<R>) -> Vec<R> {
    loop {
        let before = rects.len();
        for a in 0..2 {
            let b = 1 - a;
            rects.sort_unstable_by_key(|r| (r.min[b], r.max[b], r.min[a]));
            let mut merged: Vec<R> = Vec::with_capacity(rects.len());
            for r in rects {
                match merged.last_mut() {
                    Some(last)
                        if last.min[b] == r.min[b]
                            && last.max[b] == r.max[b]
                            && last.max[a] == r.min[a] =>
                    {
                        last.max[a] = r.max[a];
                    }
                    _ => merged.push(r),
                }
            }
            rects = merged;
        }
        if rects.len() == before {
            rects.sort_unstable();
            return rects;
        }
    }
}

impl Octree {
    /// Every mesh unit that owns a leaf drawing water or glass.
    pub(crate) fn transparent_keys(&self) -> Vec<ChunkKey> {
        let mut keys = std::collections::BTreeSet::new();
        self.for_each_leaf(|at, leaf| {
            if leaf.geometry != LeafGeometry::Full
                && volume_material_of(leaf.cube_material).is_some()
            {
                keys.insert(self.unit_key(at.min));
            }
        });
        keys.into_iter().collect()
    }

    /// The water and glass surfaces of one unit.
    pub(crate) fn transparent_unit(&self, key: ChunkKey) -> TransparentMesh {
        let mut mesh = TransparentMesh::default();
        let unit = NodeBox {
            min: key,
            size: self.unit_size(),
        };
        if !self.is_node(unit) {
            return mesh;
        }
        let mut owned = Vec::new();
        collect_owned(
            self.root_node(),
            self.root_box(),
            unit,
            &|node| drawn(node).is_some(),
            &mut owned,
        );
        let mut groups: BTreeMap<Group, Vec<R>> = BTreeMap::new();
        let mut rects = Vec::new();
        for (at, node) in owned {
            let material = drawn(node).expect("kept leaves draw");
            for k in 0..6 {
                rects.clear();
                self.face_rects(at, node, material, k, &mut rects, None);
                if !rects.is_empty() {
                    let face = Face::of(at, k);
                    groups
                        .entry((
                            face.axis,
                            face.positive,
                            face.plane,
                            material_index(material),
                        ))
                        .or_default()
                        .extend(rects.iter().copied());
                }
            }
        }
        for ((axis, positive, plane, material), rects) in groups {
            let medium = if material == 0 {
                VolumeMaterial::Water
            } else {
                VolumeMaterial::Glass
            };
            for rect in merge(rects) {
                let edges = self.edge_flags(axis, positive, plane, medium, rect);
                push_quad(&mut mesh, axis, positive, plane, material, edges, rect);
            }
        }
        mesh
    }

    /// The parts of one face of a water or glass leaf that are seen: not
    /// against the same medium, and not covered by solid geometry. One
    /// rectangle for each neighbour beyond the face that leaves a part
    /// showing, within the face. The parts solid geometry covers go to
    /// `hidden`, if given: the surface goes on under the solid.
    fn face_rects(
        &self,
        at: NodeBox,
        node: &Node,
        material: VolumeMaterial,
        k: usize,
        out: &mut Vec<R>,
        mut hidden: Option<&mut Vec<R>>,
    ) {
        let face = Face::of(at, k);
        let (u, v) = face.axes();
        if let Node::Shaped(shaped) = node {
            // The leaf's own solid may fill the whole face.
            let own = faces_on(shaped.shape, at, k);
            let side = i128::from(at.size) * 8;
            let lo = [i128::from(at.min[u]) * 8, i128::from(at.min[v]) * 8];
            let corners = [[0, 0], [side, 0], [side, side], [0, side]].map(|c: [i128; 2]| Q {
                x: lo[0] + c[0],
                y: lo[1] + c[1],
                d: 1,
            });
            let whole = R {
                min: [face.square.min[u], face.square.min[v]],
                max: [face.square.max[u], face.square.max[v]],
            };
            if !own.is_empty() && faces_hold(&own, &corners) {
                if let Some(hidden) = hidden {
                    hidden.push(whole);
                }
                return;
            }
        }
        let whole = R {
            min: [face.square.min[u], face.square.min[v]],
            max: [face.square.max[u], face.square.max[v]],
        };
        let root = self.root_box().fine_box();
        let edge = if face.positive {
            root.max[face.axis]
        } else {
            root.min[face.axis]
        };
        if face.plane == edge {
            out.push(whole);
            return;
        }
        let mut visit = |nb: NodeBox, neighbour: &Node| {
            let same = volume_material_of(neighbour.cube()) == Some(material);
            if same && hidden.is_none() {
                return true;
            }
            let covers = self.neighbour_covers(face, k, &Region::Square, nb, neighbour);
            if same && !covers {
                return true;
            }
            let b = nb.fine_box();
            let part = R {
                min: [b.min[u].max(whole.min[0]), b.min[v].max(whole.min[1])],
                max: [b.max[u].min(whole.max[0]), b.max[v].min(whole.max[1])],
            };
            if covers {
                if let Some(hidden) = hidden.as_deref_mut() {
                    hidden.push(part);
                }
            } else {
                out.push(part);
            }
            true
        };
        // Usually one leaf lies beyond the whole face: find it with one
        // descent before listing every neighbour.
        let beyond = face.beyond();
        let centre =
            std::array::from_fn(|a| (f64::from(beyond.min[a]) + f64::from(beyond.max[a])) * 0.5);
        if let Some((nb, neighbour)) = self.leaf_at_fine(centre) {
            if nb.fine_box().contains(beyond) {
                visit(nb, neighbour);
                return;
            }
        }
        self.visit_all(beyond, &mut visit);
    }

    /// The edges of a merged rectangle where its surface ends: those not
    /// continued along their whole length by faces of the same surface.
    fn edge_flags(
        &self,
        axis: usize,
        positive: bool,
        plane: i32,
        material: VolumeMaterial,
        rect: R,
    ) -> u8 {
        let k = axis * 2 + usize::from(positive);
        let axes = [(axis + 1) % 3, (axis + 2) % 3];
        // Edge numbers run (u, low), (v, low), (u, high), (v, high): the bits
        // of `EDGE_U_MIN`, `EDGE_V_MIN`, `EDGE_U_MAX` and `EDGE_V_MAX`.
        let mut flags = 0;
        for edge in 0..4 {
            let (a, high) = (edge % 2, edge / 2 == 1);
            let b = 1 - a;
            // The strip of cubes beyond the edge, one eighth deep, on the
            // inner side of the plane.
            let mut query = FineBox {
                min: [0; 3],
                max: [0; 3],
            };
            query.min[axis] = if positive { plane - 1 } else { plane };
            query.max[axis] = query.min[axis] + 1;
            let (lo, hi) = if high {
                (rect.max[a], rect.max[a] + 1)
            } else {
                (rect.min[a] - 1, rect.min[a])
            };
            query.min[axes[a]] = lo;
            query.max[axes[a]] = hi;
            query.min[axes[b]] = rect.min[b];
            query.max[axes[b]] = rect.max[b];
            let mut spans: Vec<[i32; 2]> = Vec::new();
            let mut scratch = Vec::new();
            let mut hidden = Vec::new();
            self.visit_box(query, &mut |at, node| {
                let b_box = at.fine_box();
                let on_plane = if positive {
                    b_box.max[axis] == plane
                } else {
                    b_box.min[axis] == plane
                };
                if on_plane && volume_material_of(node.cube()) == Some(material) {
                    scratch.clear();
                    self.face_rects(at, node, material, k, &mut scratch, Some(&mut hidden));
                    scratch.append(&mut hidden);
                    for r in &scratch {
                        let touches = if high {
                            r.min[a] == rect.max[a]
                        } else {
                            r.max[a] == rect.min[a]
                        };
                        if touches {
                            spans.push([r.min[b].max(rect.min[b]), r.max[b].min(rect.max[b])]);
                        }
                    }
                }
                true
            });
            spans.sort_unstable();
            let mut reached = rect.min[b];
            for [from, to] in spans {
                if from <= reached {
                    reached = reached.max(to);
                }
            }
            if reached < rect.max[b] {
                flags |= 1 << edge;
            }
        }
        flags
    }
}

fn push_quad(
    mesh: &mut TransparentMesh,
    axis: usize,
    positive: bool,
    plane: i32,
    material: u8,
    edges: u8,
    rect: R,
) {
    let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
    let mut normal = [0.0; 3];
    normal[axis] = if positive { 1.0 } else { -1.0 };
    // u cross v follows the positive axis for this cyclic axis order.
    let corners = if positive {
        [[0, 0], [1, 0], [1, 1], [0, 1]]
    } else {
        [[0, 0], [0, 1], [1, 1], [1, 0]]
    };
    let size = [
        (rect.max[0] - rect.min[0]) as f32 / 8.0,
        (rect.max[1] - rect.min[1]) as f32 / 8.0,
    ];
    for corner in corners {
        let mut position = [0.0; 3];
        position[axis] = plane as f32 / 8.0;
        position[u] = if corner[0] == 0 {
            rect.min[0]
        } else {
            rect.max[0]
        } as f32
            / 8.0;
        position[v] = if corner[1] == 0 {
            rect.min[1]
        } else {
            rect.max[1]
        } as f32
            / 8.0;
        mesh.vertices.push(TransparentVertex {
            position,
            normal,
            uv: [
                if corner[0] == 0 { 0.0 } else { size[0] },
                if corner[1] == 0 { 0.0 } else { size[1] },
            ],
            size,
            material,
            edges,
        });
    }
}
