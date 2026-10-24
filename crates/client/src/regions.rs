//! Exact unions of half-open boxes in integer eighth-metre coordinates.
//! Work scales with the number of boxes and their intersections, not world volume.
use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct RegionBox {
    pub min: [i32; 3],
    pub max: [i32; 3],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegionError {
    EmptyOrInverted,
    CoordinateOverflow,
    InvalidAxis,
}
impl fmt::Display for RegionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::EmptyOrInverted => "A volume must have positive width, height, and depth.",
            Self::CoordinateOverflow => "The transformed volume exceeds supported coordinates.",
            Self::InvalidAxis => "Choose the X, Y, or Z axis.",
        })
    }
}
impl std::error::Error for RegionError {}

impl RegionBox {
    pub fn new(min: [i32; 3], max: [i32; 3]) -> Result<Self, RegionError> {
        if (0..3).any(|axis| min[axis] >= max[axis]) {
            return Err(RegionError::EmptyOrInverted);
        }
        Ok(Self { min, max })
    }
    pub fn volume(self) -> u128 {
        (0..3)
            .map(|axis| (self.max[axis] as i64 - self.min[axis] as i64) as u128)
            .product()
    }
    #[cfg(test)]
    pub fn contains(self, point: [i32; 3]) -> bool {
        (0..3).all(|axis| self.min[axis] <= point[axis] && point[axis] < self.max[axis])
    }
    pub fn intersection(self, other: Self) -> Option<Self> {
        Self::new(
            std::array::from_fn(|i| self.min[i].max(other.min[i])),
            std::array::from_fn(|i| self.max[i].min(other.max[i])),
        )
        .ok()
    }
    pub fn translated(self, offset: [i32; 3]) -> Result<Self, RegionError> {
        let mut min = self.min;
        let mut max = self.max;
        for axis in 0..3 {
            min[axis] = min[axis]
                .checked_add(offset[axis])
                .ok_or(RegionError::CoordinateOverflow)?;
            max[axis] = max[axis]
                .checked_add(offset[axis])
                .ok_or(RegionError::CoordinateOverflow)?;
        }
        Self::new(min, max)
    }
    fn difference(self, cut: Self) -> Vec<Self> {
        let Some(overlap) = self.intersection(cut) else {
            return vec![self];
        };
        let mut remaining = self;
        let mut pieces = Vec::with_capacity(6);
        for axis in 0..3 {
            if remaining.min[axis] < overlap.min[axis] {
                let mut piece = remaining;
                piece.max[axis] = overlap.min[axis];
                pieces.push(piece);
                remaining.min[axis] = overlap.min[axis];
            }
            if remaining.max[axis] > overlap.max[axis] {
                let mut piece = remaining;
                piece.min[axis] = overlap.max[axis];
                pieces.push(piece);
                remaining.max[axis] = overlap.max[axis];
            }
        }
        pieces
    }
}
impl<'de> Deserialize<'de> for RegionBox {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            min: [i32; 3],
            max: [i32; 3],
        }
        let raw = Raw::deserialize(deserializer)?;
        Self::new(raw.min, raw.max).map_err(serde::de::Error::custom)
    }
}

/// Boxes are disjoint, sorted, and coalesced along matching complete faces.
/// This is a compact exact partition; it does not seek a globally minimum cover.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct RegionSet {
    boxes: Vec<RegionBox>,
}
impl RegionSet {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn from_boxes(boxes: impl IntoIterator<Item = RegionBox>) -> Result<Self, RegionError> {
        let mut result = Self::new();
        for bounds in boxes {
            result.add(bounds)?;
        }
        Ok(result)
    }
    pub fn boxes(&self) -> &[RegionBox] {
        &self.boxes
    }
    pub fn is_empty(&self) -> bool {
        self.boxes.is_empty()
    }
    pub fn volume(&self) -> u128 {
        self.boxes.iter().map(|b| b.volume()).sum()
    }
    #[cfg(test)]
    pub fn contains(&self, point: [i32; 3]) -> bool {
        self.boxes.iter().any(|b| b.contains(point))
    }
    pub fn covers(&self, bounds: RegionBox) -> bool {
        let mut remaining = vec![bounds];
        for &existing in &self.boxes {
            remaining = remaining
                .into_iter()
                .flat_map(|b| b.difference(existing))
                .collect();
            if remaining.is_empty() {
                return true;
            }
        }
        false
    }
    pub fn bounds(&self) -> Option<RegionBox> {
        let mut result = *self.boxes.first()?;
        for bounds in &self.boxes[1..] {
            for axis in 0..3 {
                result.min[axis] = result.min[axis].min(bounds.min[axis]);
                result.max[axis] = result.max[axis].max(bounds.max[axis]);
            }
        }
        Some(result)
    }
    pub fn add(&mut self, bounds: RegionBox) -> Result<(), RegionError> {
        RegionBox::new(bounds.min, bounds.max)?;
        let mut added = vec![bounds];
        for &existing in &self.boxes {
            added = added
                .into_iter()
                .flat_map(|b| b.difference(existing))
                .collect();
            if added.is_empty() {
                return Ok(());
            }
        }
        self.boxes.extend(added);
        self.coalesce();
        Ok(())
    }
    pub fn subtract(&mut self, bounds: RegionBox) -> Result<(), RegionError> {
        RegionBox::new(bounds.min, bounds.max)?;
        self.boxes = self
            .boxes
            .iter()
            .flat_map(|&b| b.difference(bounds))
            .collect();
        self.coalesce();
        Ok(())
    }
    pub fn translated(&self, offset: [i32; 3]) -> Result<Self, RegionError> {
        Self::from_boxes(
            self.boxes
                .iter()
                .map(|b| b.translated(offset))
                .collect::<Result<Vec<_>, _>>()?,
        )
    }
    /// Positive rotation around Y within an extent of `width` on the old X axis.
    pub fn rotated_y(&self, width: i32) -> Result<Self, RegionError> {
        let mut boxes = Vec::with_capacity(self.boxes.len());
        for b in &self.boxes {
            boxes.push(RegionBox::new(
                [
                    b.min[2],
                    b.min[1],
                    width
                        .checked_sub(b.max[0])
                        .ok_or(RegionError::CoordinateOverflow)?,
                ],
                [
                    b.max[2],
                    b.max[1],
                    width
                        .checked_sub(b.min[0])
                        .ok_or(RegionError::CoordinateOverflow)?,
                ],
            )?);
        }
        Self::from_boxes(boxes)
    }
    pub fn mirrored(&self, axis: usize, length: i32) -> Result<Self, RegionError> {
        if axis >= 3 {
            return Err(RegionError::InvalidAxis);
        }
        let mut boxes = Vec::with_capacity(self.boxes.len());
        for b in &self.boxes {
            let mut next = *b;
            next.min[axis] = length
                .checked_sub(b.max[axis])
                .ok_or(RegionError::CoordinateOverflow)?;
            next.max[axis] = length
                .checked_sub(b.min[axis])
                .ok_or(RegionError::CoordinateOverflow)?;
            boxes.push(next);
        }
        Self::from_boxes(boxes)
    }
    fn coalesce(&mut self) {
        loop {
            let before = self.boxes.len();
            for axis in 0..3 {
                let other = [(axis + 1) % 3, (axis + 2) % 3];
                self.boxes.sort_by_key(|b| {
                    (
                        b.min[other[0]],
                        b.max[other[0]],
                        b.min[other[1]],
                        b.max[other[1]],
                        b.min[axis],
                    )
                });
                let mut merged: Vec<RegionBox> = Vec::with_capacity(self.boxes.len());
                for b in self.boxes.drain(..) {
                    if let Some(last) = merged.last_mut() {
                        if other
                            .iter()
                            .all(|&i| last.min[i] == b.min[i] && last.max[i] == b.max[i])
                            && last.max[axis] == b.min[axis]
                        {
                            last.max[axis] = b.max[axis];
                            continue;
                        }
                    }
                    merged.push(b);
                }
                self.boxes = merged;
            }
            if before == self.boxes.len() {
                break;
            }
        }
        self.boxes.sort();
    }
}
impl<'de> Deserialize<'de> for RegionSet {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            boxes: Vec<RegionBox>,
        }
        Self::from_boxes(Raw::deserialize(deserializer)?.boxes).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn b(min: [i32; 3], max: [i32; 3]) -> RegionBox {
        RegionBox::new(min, max).unwrap()
    }
    #[test]
    fn million_metre_volumes_and_annex_do_not_enumerate_cells() {
        let main = b([0, 0, 0], [800, 800, 2400]);
        let annex = b([800, 0, 0], [1200, 400, 400]);
        let mut set = RegionSet::from_boxes([main, annex]).unwrap();
        assert_eq!(set.volume(), 3_125_000 * 512);
        assert_eq!(set.boxes().len(), 2);
        set.add(main).unwrap();
        assert_eq!(set.boxes().len(), 2);
        assert!(set.covers(main));
        assert!(!set.covers(set.bounds().unwrap()));
        assert!(!set.contains([900, 500, 500]));
    }
    #[test]
    fn overlaps_deduplicate_and_subtraction_preserves_a_real_hole() {
        let outer = b([-8, -8, -8], [8, 8, 8]);
        let hole = b([-2, -2, -2], [2, 2, 2]);
        let mut set = RegionSet::from_boxes([outer, b([0, 0, 0], [16, 8, 8])]).unwrap();
        assert_eq!(set.volume(), 4096 + 512);
        set.subtract(hole).unwrap();
        assert_eq!(set.volume(), 4096 + 512 - 64);
        assert!(!set.contains([0, 0, 0]));
        assert!(set.contains([-3, 0, 0]));
        for (i, a) in set.boxes().iter().enumerate() {
            for bb in &set.boxes()[i + 1..] {
                assert!(a.intersection(*bb).is_none());
            }
        }
        set.add(hole).unwrap();
        assert!(set.covers(outer));
    }
    #[test]
    fn repeated_add_subtract_matches_a_small_point_oracle() {
        let mut set = RegionSet::new();
        let mut points = std::collections::BTreeSet::new();
        for (add, box_) in [
            (true, b([-4, -3, -2], [2, 2, 3])),
            (true, b([0, -4, 0], [4, 4, 4])),
            (false, b([-1, -1, -1], [3, 3, 3])),
            (true, b([-2, 0, -2], [1, 1, 2])),
        ] {
            if add {
                set.add(box_).unwrap();
            } else {
                set.subtract(box_).unwrap();
            }
            for x in -4..4 {
                for y in -4..4 {
                    for z in -4..4 {
                        if box_.contains([x, y, z]) {
                            if add {
                                points.insert([x, y, z]);
                            } else {
                                points.remove(&[x, y, z]);
                            }
                        }
                    }
                }
            }
            assert_eq!(set.volume(), points.len() as u128);
            for x in -4..4 {
                for y in -4..4 {
                    for z in -4..4 {
                        assert_eq!(set.contains([x, y, z]), points.contains(&[x, y, z]));
                    }
                }
            }
        }
    }
    #[test]
    fn transforms_keep_masks_holes_and_volume() {
        let mut set =
            RegionSet::from_boxes([b([0, 0, 0], [8, 16, 24]), b([16, 0, 0], [24, 8, 8])]).unwrap();
        set.subtract(b([2, 2, 2], [6, 6, 6])).unwrap();
        let rotated = set.rotated_y(24).unwrap();
        assert_eq!(rotated.volume(), set.volume());
        assert!(!rotated.contains([3, 3, 20]));
        assert_eq!(
            set.mirrored(0, 24)
                .unwrap()
                .mirrored(0, 24)
                .unwrap()
                .volume(),
            set.volume()
        );
        let restored = rotated
            .rotated_y(24)
            .unwrap()
            .rotated_y(24)
            .unwrap()
            .rotated_y(24)
            .unwrap();
        for x in 0..24 {
            for y in 0..16 {
                for z in 0..24 {
                    assert_eq!(restored.contains([x, y, z]), set.contains([x, y, z]));
                }
            }
        }
    }
    #[test]
    fn serde_validates_boxes_and_normalizes_overlaps() {
        let set: RegionSet = serde_json::from_str(
            r#"{"boxes":[{"min":[0,0,0],"max":[8,8,8]},{"min":[0,0,0],"max":[4,4,4]}]}"#,
        )
        .unwrap();
        assert_eq!(set.boxes().len(), 1);
        assert_eq!(
            serde_json::from_str::<RegionSet>(&serde_json::to_string(&set).unwrap()).unwrap(),
            set
        );
        assert!(
            serde_json::from_str::<RegionSet>(r#"{"boxes":[{"min":[0,0,0],"max":[0,1,1]}]}"#)
                .is_err()
        );
        assert!(b([i32::MAX - 1, 0, 0], [i32::MAX, 1, 1])
            .translated([1, 0, 0])
            .is_err());
    }
}
