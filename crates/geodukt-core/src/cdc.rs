//! Change Data Capture — row-level change detection between feature collections.
//!
//! Compares two versions of a feature collection to produce a changeset
//! of insertions, updates, and deletions.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::feature::{Feature, FeatureCollection, Value};
use crate::geometry::{self, Coord, FeatureGeometry, Polygon};
use crate::hex;

/// The type of change detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeKind {
    Insert,
    Update,
    Delete,
}

/// A single change record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeRecord {
    pub kind: ChangeKind,
    pub key: String,
    /// For updates: which properties changed.
    pub changed_fields: Vec<String>,
}

/// A changeset between two versions of a feature collection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeSet {
    pub inserts: usize,
    pub updates: usize,
    pub deletes: usize,
    pub records: Vec<ChangeRecord>,
}

impl ChangeSet {
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn total_changes(&self) -> usize {
        self.inserts + self.updates + self.deletes
    }
}

/// CDC detector configuration.
pub struct CdcDetector {
    /// Which property to use as the primary key for matching features.
    pub key_field: String,
}

impl CdcDetector {
    pub fn new(key_field: impl Into<String>) -> Self {
        Self {
            key_field: key_field.into(),
        }
    }

    /// Compute changes between `old` and `new` feature collections.
    pub fn detect_changes(&self, old: &FeatureCollection, new: &FeatureCollection) -> ChangeSet {
        let old_map = self.index_by_key(old);
        let new_map = self.index_by_key(new);

        let mut records = Vec::new();
        let mut inserts = 0;
        let mut updates = 0;
        let mut deletes = 0;

        // Check for inserts and updates
        for (key, new_feature) in &new_map {
            match old_map.get(key) {
                None => {
                    records.push(ChangeRecord {
                        kind: ChangeKind::Insert,
                        key: key.clone(),
                        changed_fields: Vec::new(),
                    });
                    inserts += 1;
                }
                Some(old_feature) => {
                    let changed = self.diff_properties(old_feature, new_feature);
                    if !changed.is_empty() {
                        records.push(ChangeRecord {
                            kind: ChangeKind::Update,
                            key: key.clone(),
                            changed_fields: changed,
                        });
                        updates += 1;
                    }
                }
            }
        }

        // Check for deletes
        for key in old_map.keys() {
            if !new_map.contains_key(key) {
                records.push(ChangeRecord {
                    kind: ChangeKind::Delete,
                    key: key.clone(),
                    changed_fields: Vec::new(),
                });
                deletes += 1;
            }
        }

        ChangeSet {
            inserts,
            updates,
            deletes,
            records,
        }
    }

    /// Compute a content hash for a feature (geometry + properties).
    pub fn feature_hash(feature: &Feature) -> String {
        let mut hasher = Sha256::new();
        update_with_geometry(&mut hasher, &feature.geometry);
        let mut props: Vec<(&String, &Value)> = feature.properties.iter().collect();
        props.sort_by_key(|(k, _)| *k);
        update_with_length(&mut hasher, props.len());
        for (k, v) in props {
            update_with_bytes(&mut hasher, k.as_bytes());
            update_with_value(&mut hasher, v);
        }
        hex::encode_lowercase(&hasher.finalize())
    }

    fn index_by_key<'a>(&self, fc: &'a FeatureCollection) -> HashMap<String, &'a Feature> {
        let mut map = HashMap::new();
        for feature in &fc.features {
            if let Some(key_val) = feature.properties.get(&self.key_field) {
                let key = match key_val {
                    Value::String(s) => s.clone(),
                    Value::Integer(n) => n.to_string(),
                    other => format!("{other:?}"),
                };
                map.insert(key, feature);
            }
        }
        map
    }

    fn diff_properties(&self, old: &Feature, new: &Feature) -> Vec<String> {
        let mut changed = Vec::new();

        // Check all keys in new
        for (key, new_val) in &new.properties {
            match old.properties.get(key) {
                None => changed.push(key.clone()),
                Some(old_val) => {
                    if old_val != new_val {
                        changed.push(key.clone());
                    }
                }
            }
        }

        // Check keys removed from old
        for key in old.properties.keys() {
            if !new.properties.contains_key(key) {
                changed.push(key.clone());
            }
        }

        changed.sort();
        changed
    }
}

/// Feed a geometry to the hasher as an encoding geodukt owns, so an upstream
/// `Debug` change cannot move a content hash. Editing any of this moves them all.
fn update_with_geometry(hasher: &mut Sha256, geometry: &FeatureGeometry) {
    update_with_bytes(hasher, geometry::type_name(geometry).as_bytes());
    match geometry {
        FeatureGeometry::Point(point) => update_with_coord(hasher, point.0),
        FeatureGeometry::LineString(line) => update_with_coords(hasher, line.coords()),
        FeatureGeometry::Polygon(polygon) => update_with_polygon(hasher, polygon),
        FeatureGeometry::MultiPoint(points) => {
            update_with_length(hasher, points.points().len());
            for point in points.points() {
                update_with_coord(hasher, point.0);
            }
        }
        FeatureGeometry::MultiLineString(lines) => {
            update_with_length(hasher, lines.linestrings().len());
            for line in lines.linestrings() {
                update_with_coords(hasher, line.coords());
            }
        }
        FeatureGeometry::MultiPolygon(polygons) => {
            update_with_length(hasher, polygons.polygons().len());
            for polygon in polygons.polygons() {
                update_with_polygon(hasher, polygon);
            }
        }
        FeatureGeometry::GeometryCollection(members) => {
            update_with_length(hasher, members.len());
            for member in members {
                update_with_geometry(hasher, member);
            }
        }
    }
}

fn update_with_polygon(hasher: &mut Sha256, polygon: &Polygon) {
    update_with_coords(hasher, polygon.exterior().coords());
    update_with_length(hasher, polygon.interiors().len());
    for interior in polygon.interiors() {
        update_with_coords(hasher, interior.coords());
    }
}

fn update_with_coords(hasher: &mut Sha256, coords: &[Coord]) {
    update_with_length(hasher, coords.len());
    for coord in coords {
        update_with_coord(hasher, *coord);
    }
}

fn update_with_coord(hasher: &mut Sha256, coord: Coord) {
    hasher.update(coord.x.to_bits().to_be_bytes());
    hasher.update(coord.y.to_bits().to_be_bytes());
}

fn update_with_value(hasher: &mut Sha256, value: &Value) {
    match value {
        Value::Null => update_with_bytes(hasher, b"null"),
        Value::Bool(flag) => {
            update_with_bytes(hasher, b"bool");
            hasher.update([u8::from(*flag)]);
        }
        Value::Integer(number) => {
            update_with_bytes(hasher, b"integer");
            hasher.update(number.to_be_bytes());
        }
        Value::Float(number) => {
            update_with_bytes(hasher, b"float");
            hasher.update(number.to_bits().to_be_bytes());
        }
        Value::String(text) => {
            update_with_bytes(hasher, b"string");
            update_with_bytes(hasher, text.as_bytes());
        }
    }
}

/// Length prefixed so no two different structures can feed the hasher the same bytes.
fn update_with_bytes(hasher: &mut Sha256, bytes: &[u8]) {
    update_with_length(hasher, bytes.len());
    hasher.update(bytes);
}

fn update_with_length(hasher: &mut Sha256, length: usize) {
    hasher.update((length as u64).to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{FeatureGeometry, Point};

    fn make_feature(id: i64, name: &str) -> Feature {
        Feature {
            geometry: FeatureGeometry::Point(Point::new(1.0, 2.0)),
            properties: HashMap::from([
                ("id".to_string(), Value::Integer(id)),
                ("name".to_string(), Value::String(name.to_string())),
            ]),
        }
    }

    #[test]
    fn test_detect_insert() {
        let old = FeatureCollection::new(vec![make_feature(1, "a")], None);
        let new = FeatureCollection::new(vec![make_feature(1, "a"), make_feature(2, "b")], None);

        let cdc = CdcDetector::new("id");
        let changes = cdc.detect_changes(&old, &new);
        assert_eq!(changes.inserts, 1);
        assert_eq!(changes.updates, 0);
        assert_eq!(changes.deletes, 0);
    }

    #[test]
    fn test_detect_update() {
        let old = FeatureCollection::new(vec![make_feature(1, "a")], None);
        let new = FeatureCollection::new(vec![make_feature(1, "a_modified")], None);

        let cdc = CdcDetector::new("id");
        let changes = cdc.detect_changes(&old, &new);
        assert_eq!(changes.inserts, 0);
        assert_eq!(changes.updates, 1);
        assert_eq!(changes.deletes, 0);
        assert_eq!(changes.records[0].changed_fields, vec!["name"]);
    }

    #[test]
    fn test_detect_delete() {
        let old = FeatureCollection::new(vec![make_feature(1, "a"), make_feature(2, "b")], None);
        let new = FeatureCollection::new(vec![make_feature(1, "a")], None);

        let cdc = CdcDetector::new("id");
        let changes = cdc.detect_changes(&old, &new);
        assert_eq!(changes.inserts, 0);
        assert_eq!(changes.updates, 0);
        assert_eq!(changes.deletes, 1);
    }

    #[test]
    fn test_no_changes() {
        let fc = FeatureCollection::new(vec![make_feature(1, "a")], None);
        let cdc = CdcDetector::new("id");
        let changes = cdc.detect_changes(&fc, &fc);
        assert!(changes.is_empty());
    }

    #[test]
    fn test_feature_hash_golden() {
        let feature = Feature {
            geometry: FeatureGeometry::Point(Point::new(1.5, -2.25)),
            properties: HashMap::from([
                ("id".to_string(), Value::Integer(42)),
                ("name".to_string(), Value::String("golden".to_string())),
            ]),
        };
        assert_eq!(
            CdcDetector::feature_hash(&feature),
            "d933855d1b34f1b4f26ffbc744006d51830ae87bdca830a247c6d1b8b15fc2e4"
        );
    }

    #[test]
    fn test_feature_hash() {
        let f1 = make_feature(1, "a");
        let f2 = make_feature(1, "b");
        assert_ne!(
            CdcDetector::feature_hash(&f1),
            CdcDetector::feature_hash(&f2)
        );
    }

    #[test]
    fn test_feature_hash_separates_geometries_sharing_coordinates() {
        use crate::geometry::{LineString, MultiPoint};

        let coords = [Coord::new(0.0, 0.0), Coord::new(1.0, 1.0)];
        let line = FeatureGeometry::LineString(LineString::new(coords.to_vec()));
        let points = FeatureGeometry::MultiPoint(MultiPoint::new(coords.map(Point).to_vec()));
        let hash_of = |geometry| {
            CdcDetector::feature_hash(&Feature {
                geometry,
                properties: HashMap::new(),
            })
        };
        assert_ne!(hash_of(line), hash_of(points));
    }
}
