//! Format 3 at the tree's boundary. Worlds saved in versions 1–3 load into the
//! tree; the tree writes format 3 that builds from before the octree open:
//! leaves as `regions`, one-metre leaves on whole metres as `cells`,
//! height-field shapes as `heights` and `diagonal`, and water and glass as
//! boxes.

use super::{
    cube_material_for, lattice, relation, restrict, root_for, volume_material_of, Leaf,
    LeafGeometry, NodeBox, Octree, OctreeBuilder, Relation, Source, MAT_AIR, MAX_CONVERSION_LEAVES,
};
use crate::{
    coalesce_material_volumes, validate_material_volumes, validate_root_edits, Cell, CellEdit,
    CellFragment, CellPos, FineBox, MaterialVolume, RootEdit, Shape, World, WorldBounds,
};
use std::collections::BTreeSet;

/// A world's geometry in format 3's parts.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Format3 {
    pub bounds: WorldBounds,
    pub regions: Vec<CellFragment>,
    pub cells: Vec<CellEdit>,
    pub roots: Vec<RootEdit>,
    pub volumes: Vec<MaterialVolume>,
}

fn faces_of(cell: &Cell) -> [u8; 6] {
    std::array::from_fn(|face| cell.material_for_face(face as u8))
}

fn source_of(origin: [i32; 3], size: u32, cell: &Cell) -> Source {
    Source {
        origin,
        size,
        shape: if cell.is_full() {
            Shape::solid()
        } else {
            cell.as_shape()
        },
    }
}

pub(super) fn cell_for(shape: Option<Shape>, faces: [u8; 6]) -> Cell {
    let material = faces[0];
    let mut cell = Cell {
        material,
        face_materials: faces.iter().any(|&f| f != material).then_some(faces),
        heights: [8; 4],
        diagonal: false,
        shape: None,
    };
    if let Some(shape) = shape {
        match lattice::heightfield_form(shape) {
            Some((heights, diagonal)) => {
                cell.heights = heights;
                cell.diagonal = diagonal;
            }
            None => cell.shape = Some(shape),
        }
    }
    cell
}

/// The root for converted cubes: at the bounds' minimum corner when the
/// largest whole region lies on that grid, otherwise on the largest whole
/// region's grid just below the bounds, so an imported map's own octree keeps
/// its leaves whole.
fn choose_root(bounds: WorldBounds, regions: &[CellFragment]) -> Result<NodeBox, String> {
    let fine = bounds.fine_box();
    let anchor = regions
        .iter()
        .filter(|r| r.source_bounds() == Some(r.bounds))
        .max_by(|a, b| a.size.cmp(&b.size).then(b.origin.cmp(&a.origin)));
    let origin = match anchor {
        None => fine.min,
        Some(region) => {
            let size = region.size as i32;
            std::array::from_fn(|a| fine.min[a] - (fine.min[a] - region.origin[a]).rem_euclid(size))
        }
    };
    root_for(bounds, origin)
}

/// The maximal nodes of a tree's grid inside a box.
fn decompose(
    root: NodeBox,
    target: FineBox,
    budget: usize,
    out: &mut Vec<NodeBox>,
) -> Result<(), String> {
    fn walk(
        at: NodeBox,
        target: FineBox,
        budget: usize,
        out: &mut Vec<NodeBox>,
    ) -> Result<(), String> {
        match relation(at, target) {
            Relation::Disjoint => Ok(()),
            Relation::Inside => {
                if out.len() >= budget {
                    return Err("The world expands past the tree's conversion limit.".into());
                }
                out.push(at);
                Ok(())
            }
            Relation::Partial => {
                for i in 0..8 {
                    walk(at.child(i), target, budget, out)?;
                }
                Ok(())
            }
        }
    }
    walk(root, target, budget, out)
}

impl Octree {
    /// Loads a world saved in format 1, 2 or 3, after the same validation as
    /// `World::from_compact_parts` and `World::replace_volumes`.
    pub fn from_format3(world: &Format3) -> Result<Self, String> {
        let bounds = world.bounds;
        if !bounds.is_valid() {
            return Err("Invalid world bounds (maximum 8192 metres per axis).".into());
        }
        let fine = bounds.fine_box();
        for region in &world.regions {
            if !region.is_valid() || !fine.contains(region.bounds) {
                return Err("Invalid compact region bounds or geometry.".into());
            }
        }
        let mut seen = BTreeSet::new();
        for cell in &world.cells {
            if !bounds.contains_pos(cell.pos)
                || cell.cell.is_some_and(|c| !c.is_valid())
                || !seen.insert(cell.pos)
            {
                return Err("Invalid or repeated coarse cell.".into());
            }
        }
        for root in &world.roots {
            if !seen.insert(root.pos) {
                return Err("A saved root has repeated geometry.".into());
            }
        }
        validate_root_edits(&world.roots)?;
        if world.roots.iter().any(|r| !bounds.contains_pos(r.pos)) {
            return Err("Edit lies outside this world.".into());
        }
        validate_material_volumes(&world.volumes, fine)?;

        let root_box = choose_root(bounds, &world.regions)?;
        let mut builder = OctreeBuilder::new(bounds, root_box)?;
        let mut nodes = Vec::new();
        let add = |builder: &mut OctreeBuilder,
                   nodes: &mut Vec<NodeBox>,
                   clip: FineBox,
                   source: Source,
                   faces: [u8; 6]|
         -> Result<(), String> {
            let at = NodeBox {
                min: source.origin,
                size: source.size,
            };
            if clip == at.fine_box() && builder.tree.is_node(at) {
                let geometry = if lattice::is_solid(source.shape) {
                    LeafGeometry::Full
                } else {
                    LeafGeometry::Shaped(source.shape)
                };
                return builder.push(
                    at,
                    Leaf {
                        geometry,
                        faces,
                        cube_material: MAT_AIR,
                    },
                );
            }
            let (source, visible) = builder.shapes.source(source)?;
            let faces = lattice::fill_faces(visible, faces);
            nodes.clear();
            decompose(
                root_box,
                clip,
                MAX_CONVERSION_LEAVES.saturating_sub(builder.len()),
                nodes,
            )?;
            for &nb in nodes.iter() {
                builder.add(nb, restrict(source, faces, MAT_AIR, nb))?;
            }
            Ok(())
        };
        for region in &world.regions {
            add(
                &mut builder,
                &mut nodes,
                region.bounds,
                source_of(region.origin, region.size, &region.cell),
                faces_of(&region.cell),
            )?;
        }
        let overrides = !world.regions.is_empty();
        if !overrides {
            for cell in &world.cells {
                if let Some(c) = cell.cell {
                    let root = FineBox::root(cell.pos);
                    add(
                        &mut builder,
                        &mut nodes,
                        root,
                        source_of(root.min, 8, &c),
                        faces_of(&c),
                    )?;
                }
            }
            for root in &world.roots {
                for f in &root.fragments {
                    add(
                        &mut builder,
                        &mut nodes,
                        f.bounds,
                        source_of(f.origin, f.size, &f.cell),
                        faces_of(&f.cell),
                    )?;
                }
            }
        }
        let mut tree = builder.finish()?;
        if overrides {
            // Cells and roots replace the regions in their metre.
            for cell in &world.cells {
                let root = FineBox::root(cell.pos);
                tree.clear_geometry(root);
                if let Some(c) = cell.cell {
                    tree.insert_source(root, source_of(root.min, 8, &c), faces_of(&c));
                }
            }
            let mut shapes = super::ShapeCache::default();
            for root in &world.roots {
                tree.clear_geometry(FineBox::root(root.pos));
                for f in &root.fragments {
                    let (source, _) = shapes.source(source_of(f.origin, f.size, &f.cell))?;
                    tree.insert_source(f.bounds, source, faces_of(&f.cell));
                }
            }
        }
        let mut budget = MAX_CONVERSION_LEAVES.saturating_sub(tree.stats().leaves());
        for volume in &world.volumes {
            nodes.clear();
            decompose(root_box, volume.bounds, budget, &mut nodes)?;
            budget -= nodes.len();
        }
        for volume in &world.volumes {
            tree.set_cube_material(volume.bounds, cube_material_for(volume.material));
        }
        Ok(tree)
    }

    /// Converts a `World` through its snapshot's format 3 parts.
    pub fn from_world(world: &World) -> Result<Self, String> {
        let snapshot = world.snapshot();
        Self::from_format3(&Format3 {
            bounds: snapshot.bounds(),
            regions: snapshot.regions().to_vec(),
            cells: snapshot.cells(),
            roots: snapshot.root_overrides(),
            volumes: snapshot.volumes().to_vec(),
        })
    }

    /// Format 3 for files and messages. Builds from before the octree open it
    /// within their own limits as long as the world stays within them.
    pub fn to_format3(&self) -> Format3 {
        let (regions, cells) = self.geometry_parts();
        let mut boxes = Vec::new();
        self.for_each_leaf(|at, leaf| {
            if let Some(material) = volume_material_of(leaf.cube_material) {
                boxes.push(MaterialVolume {
                    bounds: at.fine_box(),
                    material,
                });
            }
        });
        Format3 {
            bounds: self.bounds,
            regions,
            cells,
            roots: Vec::new(),
            volumes: coalesce_material_volumes(boxes),
        }
    }

    fn geometry_parts(&self) -> (Vec<CellFragment>, Vec<CellEdit>) {
        let world = self.bounds.fine_box();
        let mut regions = Vec::new();
        let mut cells = Vec::new();
        self.for_each_leaf(|at, leaf| {
            let (origin, size, shape) = match leaf.geometry {
                LeafGeometry::Empty => return,
                LeafGeometry::Full => (at.min, at.size, None),
                LeafGeometry::Shaped(shape) => (at.min, at.size, Some(shape)),
                LeafGeometry::Piece(source) => (source.origin, source.size, Some(source.shape)),
            };
            let cell = cell_for(shape, leaf.faces);
            let plain = !matches!(leaf.geometry, LeafGeometry::Piece(_));
            if plain && at.size == 8 && at.min.iter().all(|v| v.rem_euclid(8) == 0) {
                cells.push(CellEdit {
                    pos: at.min.map(|v| v.div_euclid(8)),
                    cell: Some(cell),
                });
            } else {
                // A merged piece's cube can cross the bounds while its solid
                // stays inside them.
                let bounds = at.fine_box().intersection(world).unwrap();
                regions.push(CellFragment {
                    bounds,
                    origin,
                    size,
                    cell,
                });
            }
        });
        cells.sort_by_key(|c| c.pos);
        (regions, cells)
    }

    /// The leaves format 3 writes as compact regions.
    pub fn regions(&self) -> Vec<CellFragment> {
        self.geometry_parts().0
    }

    /// One-metre leaves on whole metres, which format 3 writes as cells.
    pub fn cells(&self) -> Vec<CellEdit> {
        self.geometry_parts().1
    }

    /// The tree has no masks or overrides, so format 3 needs no roots.
    pub fn root_overrides(&self) -> Vec<RootEdit> {
        Vec::new()
    }

    pub fn region_count(&self) -> usize {
        let mut count = 0;
        self.for_each_leaf(|at, leaf| {
            let cell = !matches!(leaf.geometry, LeafGeometry::Piece(_))
                && at.size == 8
                && at.min.iter().all(|v| v.rem_euclid(8) == 0);
            if leaf.geometry != LeafGeometry::Empty && !cell {
                count += 1;
            }
        });
        count
    }

    pub fn override_count(&self) -> usize {
        0
    }

    /// The coarse cell filling a whole metre: a one-metre shaped leaf on whole
    /// metres, or any metre of a full leaf.
    pub fn get(&self, pos: CellPos) -> Option<Cell> {
        if !self.bounds.contains_pos(pos) {
            return None;
        }
        let metre = NodeBox {
            min: pos.map(|v| v * 8),
            size: 8,
        };
        let (at, leaf) = self.leaf_containing_box(metre.fine_box())?;
        match leaf.geometry {
            LeafGeometry::Full => Some(cell_for(None, leaf.faces)),
            LeafGeometry::Shaped(shape) if at == metre => Some(cell_for(Some(shape), leaf.faces)),
            _ => None,
        }
    }

    /// The leaf that holds a whole fine box, if one does.
    pub(crate) fn leaf_containing_box(&self, target: FineBox) -> Option<(NodeBox, Leaf)> {
        let mut node = self.root_node();
        let mut at = self.root_box();
        if !at.fine_box().contains(target) {
            return None;
        }
        loop {
            match node {
                super::Node::Branch(children) => {
                    let index = (0..8).find(|&i| at.child(i).fine_box().contains(target))?;
                    node = &children[index];
                    at = at.child(index);
                }
                leaf => return Some((at, leaf.leaf().unwrap())),
            }
        }
    }

    /// Metres that hold detail below a metre, with their content.
    pub fn detailed_roots(&self) -> Vec<RootEdit> {
        let mut positions = BTreeSet::new();
        self.for_each_leaf(|at, leaf| {
            if at.size < 8 && leaf.geometry != LeafGeometry::Empty {
                positions.insert(at.min.map(|v| v.div_euclid(8)));
            }
        });
        positions
            .into_iter()
            .filter_map(|pos| {
                let fragments = self.root_fragments(pos);
                (!fragments.is_empty()).then_some(RootEdit { pos, fragments })
            })
            .collect()
    }
}

/// Builds a canonical tree node from leaves relative to a node, for tests and
/// writes: every leaf must sit on a node inside `at`.
#[cfg(test)]
pub(crate) fn subtree(
    bounds: WorldBounds,
    root: NodeBox,
    at: NodeBox,
    leaves: &[(NodeBox, Leaf)],
) -> Result<super::Node, String> {
    let mut tree = Octree::with_root(bounds, root)?;
    for &(nb, leaf) in leaves {
        if !at.contains(nb) {
            return Err("A subtree leaf lies outside its node.".into());
        }
        let node = super::make_leaf(nb, leaf)?;
        tree.modify(nb.fine_box(), &mut |_, inner| {
            super::restrict_node(&node, nb, inner)
        });
    }
    Ok(tree.node_at(at))
}
