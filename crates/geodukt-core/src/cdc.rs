//! Change Data Capture — row-level change detection between feature collections.
//!
//! Compares two versions of a feature collection to produce a changeset
//! of insertions, updates, and deletions.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::feature::{Feature, FeatureCollection, Value};

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
        hasher.update(format!("{:?}", feature.geometry).as_bytes());
        let mut props: Vec<(&String, &Value)> = feature.properties.iter().collect();
        props.sort_by_key(|(k, _)| *k);
        for (k, v) in props {
            hasher.update(format!("{k}={v:?}").as_bytes());
        }
        format!("{:x}", hasher.finalize())
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
    fn test_feature_hash() {
        let f1 = make_feature(1, "a");
        let f2 = make_feature(1, "b");
        assert_ne!(
            CdcDetector::feature_hash(&f1),
            CdcDetector::feature_hash(&f2)
        );
    }
}
