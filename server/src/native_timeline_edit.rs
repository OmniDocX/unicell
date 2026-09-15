//! Lossless reader/editor for native Excel 2013+ Timeline parts.
//!
//! A Timeline is split across two independent native OOXML parts:
//!
//! * a workbook-owned `timelineCacheDefinition`, which stores the date bounds, current
//!   selection, filter state, and the PivotTables filtered by the Timeline; and
//! * a worksheet-owned `timelines` part, which stores view properties such as caption,
//!   level, scroll position, visibility flags, and style.
//!
//! Both parts are reached through explicit OPC relationships.  This module deliberately does
//! not depend on conventional names such as `timeline1.xml`: it resolves the relationship graph
//! and edits the actual target parts.  Edits are differential.  Existing XML is retained as the
//! source of truth and only requested start-tag attributes or known elements are changed.  An
//! empty or semantically equal patch is therefore byte-for-byte identical, while unknown
//! attributes, children, namespace declarations, comments, extension lists, and vendor payloads
//! survive every supported edit.

use roxmltree::{Document, Node};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;

const REL_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const MAIN_NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PatchField<T> {
    Missing,
    Null,
    Value(T),
}

impl<T> Default for PatchField<T> {
    fn default() -> Self {
        Self::Missing
    }
}

impl<T> PatchField<T> {
    fn is_missing(&self) -> bool {
        matches!(self, Self::Missing)
    }
}

impl<T: Serialize> Serialize for PatchField<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Missing | Self::Null => serializer.serialize_none(),
            Self::Value(value) => value.serialize(serializer),
        }
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for PatchField<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(match Option::<T>::deserialize(deserializer)? {
            Some(value) => Self::Value(value),
            None => Self::Null,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct TimelineRange {
    pub start_date: String,
    pub end_date: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct TimelineConnectionTarget {
    pub tab_id: u32,
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct TimelineConnectionDelta {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub add: Vec<TimelineConnectionTarget>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remove: Vec<TimelineConnectionTarget>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct TimelineViewPatch {
    #[serde(default, skip_serializing_if = "PatchField::is_missing")]
    pub caption: PatchField<String>,
    #[serde(default, skip_serializing_if = "PatchField::is_missing")]
    pub level: PatchField<u32>,
    #[serde(default, skip_serializing_if = "PatchField::is_missing")]
    pub selection_level: PatchField<u32>,
    #[serde(default, skip_serializing_if = "PatchField::is_missing")]
    pub scroll_position: PatchField<String>,
    #[serde(default, skip_serializing_if = "PatchField::is_missing")]
    pub style: PatchField<String>,
    #[serde(default, skip_serializing_if = "PatchField::is_missing")]
    pub show_header: PatchField<bool>,
    #[serde(default, skip_serializing_if = "PatchField::is_missing")]
    pub show_selection_label: PatchField<bool>,
    #[serde(default, skip_serializing_if = "PatchField::is_missing")]
    pub show_time_level: PatchField<bool>,
    #[serde(default, skip_serializing_if = "PatchField::is_missing")]
    pub show_horizontal_scrollbar: PatchField<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct TimelineCachePatch {
    #[serde(default, skip_serializing_if = "PatchField::is_missing")]
    pub selection: PatchField<TimelineRange>,
    #[serde(default, skip_serializing_if = "PatchField::is_missing")]
    pub bounds: PatchField<TimelineRange>,
    #[serde(default, skip_serializing_if = "PatchField::is_missing")]
    pub filter_type: PatchField<String>,
    #[serde(default, skip_serializing_if = "PatchField::is_missing")]
    pub single_range_filter_state: PatchField<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connections: Option<TimelineConnectionDelta>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub clear_moving_period_state: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub clear_timeline_pivot_filter: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct TimelineViewEditTarget {
    pub part: String,
    pub name: String,
    pub patch: TimelineViewPatch,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct TimelineCacheEditTarget {
    pub part: String,
    pub patch: TimelineCachePatch,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct TimelineEditRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<TimelineViewEditTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<TimelineCacheEditTarget>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PivotCacheAudit {
    pub cache_id: u32,
    pub relationship_id: String,
    pub part: Option<String>,
    pub pivot_cache_id: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PivotTableAudit {
    pub sheet: String,
    pub sheet_id: u32,
    pub sheet_part: String,
    pub name: String,
    pub part: String,
    pub relationship_id: String,
    pub cache_id: Option<u32>,
    pub cache_part: Option<String>,
    pub created_version: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TimelineConnectionModel {
    pub tab_id: u32,
    pub name: String,
    pub sheet: Option<String>,
    pub pivot_table_part: Option<String>,
    pub pivot_cache_part: Option<String>,
    pub exists: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TimelineStateModel {
    pub single_range_filter_state: Option<bool>,
    pub minimal_refresh_version: Option<u32>,
    pub last_refresh_version: Option<u32>,
    pub pivot_cache_id: Option<u32>,
    pub pivot_cache_part: Option<String>,
    pub filter_type: Option<String>,
    pub filter_id: Option<u32>,
    pub filter_tab_id: Option<u32>,
    pub filter_pivot_name: Option<String>,
    pub selection: Option<TimelineRange>,
    pub bounds: Option<TimelineRange>,
    pub has_moving_period_state: bool,
    pub has_timeline_pivot_filter: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TimelineCacheModel {
    pub part: String,
    pub relationship_id: String,
    pub name: Option<String>,
    pub uid: Option<String>,
    pub source_name: Option<String>,
    pub state: TimelineStateModel,
    pub connections: Vec<TimelineConnectionModel>,
    pub view_parts: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TimelineViewModel {
    pub part: String,
    pub relationship_id: String,
    pub sheet: String,
    pub sheet_id: u32,
    pub sheet_part: String,
    pub name: Option<String>,
    pub uid: Option<String>,
    pub cache: Option<String>,
    pub cache_part: Option<String>,
    pub caption: Option<String>,
    pub show_header: Option<bool>,
    pub show_selection_label: Option<bool>,
    pub show_time_level: Option<bool>,
    pub show_horizontal_scrollbar: Option<bool>,
    pub level: Option<u32>,
    pub selection_level: Option<u32>,
    pub scroll_position: Option<String>,
    pub style: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TimelineWorkbookModel {
    pub workbook_part: String,
    pub date_1904: bool,
    pub timeline_cache_pivot_cache_ids: Vec<u32>,
    pub pivot_caches: Vec<PivotCacheAudit>,
    pub pivot_tables: Vec<PivotTableAudit>,
    pub caches: Vec<TimelineCacheModel>,
    pub views: Vec<TimelineViewModel>,
}

#[derive(Clone, Debug)]
struct Relationship {
    id: String,
    kind: String,
    resolved_part: Option<String>,
}

#[derive(Clone, Debug)]
struct SheetInfo {
    name: String,
    id: u32,
    part: String,
}

#[derive(Clone, Debug)]
struct AttributeSpan {
    name: String,
    value_range: Range<usize>,
    full_range: Range<usize>,
}

fn local_name<'a, 'input>(node: Node<'a, 'input>) -> &'a str {
    node.tag_name().name()
}

fn direct_child<'a, 'input>(node: Node<'a, 'input>, name: &str) -> Option<Node<'a, 'input>> {
    node.children()
        .find(|child| child.is_element() && local_name(*child) == name)
}

fn direct_children<'a, 'input>(
    node: Node<'a, 'input>,
    name: &'a str,
) -> impl Iterator<Item = Node<'a, 'input>> {
    node.children()
        .filter(move |child| child.is_element() && local_name(*child) == name)
}

fn relationship_id(node: Node<'_, '_>) -> Option<String> {
    node.attributes()
        .find(|attribute| attribute.name() == "id" && attribute.namespace() == Some(REL_NS))
        .map(|attribute| attribute.value().to_string())
        .or_else(|| node.attribute("id").map(str::to_string))
}

fn relationship_kind_is(kind: &str, terminal: &str) -> bool {
    kind.rsplit('/')
        .next()
        .is_some_and(|value| value.eq_ignore_ascii_case(terminal))
}

fn normalize_part_path(path: &str) -> Option<String> {
    let path = path.replace('\\', "/");
    let mut components = Vec::new();
    for component in path.trim_start_matches('/').split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components.pop()?;
            }
            _ => components.push(component),
        }
    }
    (!components.is_empty()).then(|| components.join("/"))
}

fn part_directory(part: &str) -> &str {
    part.rsplit_once('/')
        .map(|(directory, _)| directory)
        .unwrap_or("")
}

fn resolve_relationship_target(owner: &str, target: &str) -> Option<String> {
    let target = target.split('#').next().unwrap_or(target);
    if target.starts_with('/') {
        normalize_part_path(target)
    } else {
        let directory = part_directory(owner);
        if directory.is_empty() {
            normalize_part_path(target)
        } else {
            normalize_part_path(&format!("{directory}/{target}"))
        }
    }
}

fn relationship_part(owner: &str) -> String {
    match owner.rsplit_once('/') {
        Some((directory, file)) => format!("{directory}/_rels/{file}.rels"),
        None => format!("_rels/{owner}.rels"),
    }
}

fn parse_relationships(
    parts: &BTreeMap<String, Vec<u8>>,
    owner: &str,
) -> Result<HashMap<String, Relationship>, String> {
    let path = if owner.is_empty() {
        "_rels/.rels".to_string()
    } else {
        relationship_part(owner)
    };
    let Some(bytes) = parts.get(&path) else {
        return Ok(HashMap::new());
    };
    let xml = std::str::from_utf8(bytes).map_err(|error| format!("{path} UTF-8: {error}"))?;
    let document = Document::parse(xml).map_err(|error| format!("{path} XML: {error}"))?;
    let mut relationships = HashMap::new();
    for node in document
        .descendants()
        .filter(|node| node.is_element() && local_name(*node) == "Relationship")
    {
        let (Some(id), Some(target)) = (node.attribute("Id"), node.attribute("Target")) else {
            continue;
        };
        let external = node
            .attribute("TargetMode")
            .is_some_and(|mode| mode.eq_ignore_ascii_case("External"));
        relationships.insert(
            id.to_string(),
            Relationship {
                id: id.to_string(),
                kind: node.attribute("Type").unwrap_or("").to_string(),
                resolved_part: (!external)
                    .then(|| resolve_relationship_target(owner, target))
                    .flatten(),
            },
        );
    }
    Ok(relationships)
}

fn office_document_part(parts: &BTreeMap<String, Vec<u8>>) -> Result<String, String> {
    if let Some(part) = parse_relationships(parts, "")?
        .values()
        .find(|relationship| relationship_kind_is(&relationship.kind, "officeDocument"))
        .and_then(|relationship| relationship.resolved_part.clone())
        .filter(|part| parts.contains_key(part))
    {
        return Ok(part);
    }
    if parts.contains_key("xl/workbook.xml") {
        Ok("xl/workbook.xml".to_string())
    } else {
        Err("OPC package has no workbook part".to_string())
    }
}

fn xml_part<'a>(parts: &'a BTreeMap<String, Vec<u8>>, path: &str) -> Result<&'a str, String> {
    let bytes = parts
        .get(path)
        .ok_or_else(|| format!("missing OPC part {path}"))?;
    std::str::from_utf8(bytes).map_err(|error| format!("{path} UTF-8: {error}"))
}

fn bool_attribute(node: Node<'_, '_>, name: &str) -> Option<bool> {
    parse_bool(node.attribute(name)?)
}

fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "1" | "true" | "on" => Some(true),
        "0" | "false" | "off" => Some(false),
        _ => None,
    }
}

fn u32_attribute(node: Node<'_, '_>, name: &str) -> Option<u32> {
    node.attribute(name)?.parse().ok()
}

fn uid_attribute(node: Node<'_, '_>) -> Option<String> {
    node.attributes()
        .find(|attribute| attribute.name() == "uid")
        .map(|attribute| attribute.value().to_string())
}

fn sheets(
    parts: &BTreeMap<String, Vec<u8>>,
    workbook: Node<'_, '_>,
    workbook_relationships: &HashMap<String, Relationship>,
) -> Vec<SheetInfo> {
    let mut result = Vec::new();
    for sheet in workbook
        .descendants()
        .filter(|node| node.is_element() && local_name(*node) == "sheet")
    {
        let (Some(name), Some(id), Some(relationship_id)) = (
            sheet.attribute("name"),
            u32_attribute(sheet, "sheetId"),
            relationship_id(sheet),
        ) else {
            continue;
        };
        let Some(relationship) = workbook_relationships.get(&relationship_id) else {
            continue;
        };
        if !relationship_kind_is(&relationship.kind, "worksheet") {
            continue;
        }
        let Some(part) = relationship
            .resolved_part
            .as_ref()
            .filter(|part| parts.contains_key(*part))
        else {
            continue;
        };
        result.push(SheetInfo {
            name: name.to_string(),
            id,
            part: part.clone(),
        });
    }
    result.sort_by_key(|sheet| sheet.id);
    result
}

fn parse_pivot_caches(
    parts: &BTreeMap<String, Vec<u8>>,
    workbook: Node<'_, '_>,
    relationships: &HashMap<String, Relationship>,
) -> Result<Vec<PivotCacheAudit>, String> {
    let mut result = Vec::new();
    for container in workbook.descendants().filter(|node| {
        node.is_element()
            && local_name(*node) == "pivotCaches"
            && node.tag_name().namespace() == Some(MAIN_NS)
    }) {
        for cache in direct_children(container, "pivotCache") {
            let (Some(cache_id), Some(relationship_id)) =
                (u32_attribute(cache, "cacheId"), relationship_id(cache))
            else {
                continue;
            };
            let relationship = relationships.get(&relationship_id);
            let part = relationship
                .filter(|relationship| {
                    relationship_kind_is(&relationship.kind, "pivotCacheDefinition")
                })
                .and_then(|relationship| relationship.resolved_part.clone())
                .filter(|part| parts.contains_key(part));
            let pivot_cache_id = if let Some(part) = part.as_deref() {
                let document = Document::parse(xml_part(parts, part)?)
                    .map_err(|error| format!("{part} XML: {error}"))?;
                // Excel writes the Timeline/Slicer cache identifier on the x14
                // `pivotCacheDefinition` child in extLst, rather than on the standard root.
                // Some producers do use a root attribute, so accept both without depending on
                // the namespace prefix chosen by either producer.
                u32_attribute(document.root_element(), "pivotCacheId").or_else(|| {
                    document
                        .descendants()
                        .filter(|node| {
                            node.is_element() && local_name(*node) == "pivotCacheDefinition"
                        })
                        .find_map(|node| u32_attribute(node, "pivotCacheId"))
                })
            } else {
                None
            };
            result.push(PivotCacheAudit {
                cache_id,
                relationship_id,
                part,
                pivot_cache_id,
            });
        }
    }
    result.sort_by_key(|cache| cache.cache_id);
    Ok(result)
}

fn pivot_cache_target(
    parts: &BTreeMap<String, Vec<u8>>,
    pivot_table_part: &str,
) -> Result<Option<String>, String> {
    Ok(parse_relationships(parts, pivot_table_part)?
        .values()
        .find(|relationship| relationship_kind_is(&relationship.kind, "pivotCacheDefinition"))
        .and_then(|relationship| relationship.resolved_part.clone()))
}

fn parse_pivot_tables(
    parts: &BTreeMap<String, Vec<u8>>,
    sheets: &[SheetInfo],
) -> Result<Vec<PivotTableAudit>, String> {
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    for sheet in sheets {
        let relationships = parse_relationships(parts, &sheet.part)?;
        // Excel normally emits `pivotTableParts/pivotTablePart`, but real Excel 16 files can
        // retain only the explicit worksheet relationship after Timeline/Slicer edits.  The
        // relationship itself is normative, so audit every local PivotTable relationship.
        for relationship in relationships
            .values()
            .filter(|relationship| relationship_kind_is(&relationship.kind, "pivotTable"))
        {
            let Some(part) = relationship
                .resolved_part
                .as_ref()
                .filter(|part| parts.contains_key(*part))
            else {
                continue;
            };
            if !seen.insert(part.clone()) {
                continue;
            }
            let pivot_document = Document::parse(xml_part(parts, part)?)
                .map_err(|error| format!("{part} XML: {error}"))?;
            let root = pivot_document.root_element();
            if local_name(root) != "pivotTableDefinition" {
                continue;
            }
            let Some(name) = root.attribute("name") else {
                continue;
            };
            result.push(PivotTableAudit {
                sheet: sheet.name.clone(),
                sheet_id: sheet.id,
                sheet_part: sheet.part.clone(),
                name: name.to_string(),
                part: part.clone(),
                relationship_id: relationship.id.clone(),
                cache_id: u32_attribute(root, "cacheId"),
                cache_part: pivot_cache_target(parts, part)?,
                created_version: u32_attribute(root, "createdVersion"),
            });
        }
    }
    result.sort_by(|left, right| {
        left.sheet_id
            .cmp(&right.sheet_id)
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(result)
}

fn timeline_cache_pivot_cache_ids(workbook: Node<'_, '_>) -> Vec<u32> {
    let mut result = Vec::new();
    for container in workbook
        .descendants()
        .filter(|node| node.is_element() && local_name(*node) == "timelineCachePivotCaches")
    {
        for cache in container
            .descendants()
            .filter(|node| node.is_element() && local_name(*node) == "pivotCache")
        {
            if let Some(cache_id) = u32_attribute(cache, "cacheId") {
                if !result.contains(&cache_id) {
                    result.push(cache_id);
                }
            }
        }
    }
    result.sort_unstable();
    result
}

fn range_model(node: Option<Node<'_, '_>>) -> Option<TimelineRange> {
    let node = node?;
    Some(TimelineRange {
        start_date: node.attribute("startDate")?.to_string(),
        end_date: node.attribute("endDate")?.to_string(),
    })
}

fn pivot_cache_part_for_state(id: Option<u32>, pivot_caches: &[PivotCacheAudit]) -> Option<String> {
    let id = id?;
    pivot_caches
        .iter()
        .find(|cache| cache.pivot_cache_id == Some(id))
        .or_else(|| pivot_caches.iter().find(|cache| cache.cache_id == id))
        .and_then(|cache| cache.part.clone())
}

fn parse_timeline_caches(
    parts: &BTreeMap<String, Vec<u8>>,
    workbook: Node<'_, '_>,
    relationships: &HashMap<String, Relationship>,
    pivot_caches: &[PivotCacheAudit],
    pivot_tables: &[PivotTableAudit],
) -> Result<Vec<TimelineCacheModel>, String> {
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    for reference in workbook
        .descendants()
        .filter(|node| node.is_element() && local_name(*node) == "timelineCacheRef")
    {
        let Some(relationship_id) = relationship_id(reference) else {
            continue;
        };
        let Some(relationship) = relationships.get(&relationship_id) else {
            continue;
        };
        if !relationship_kind_is(&relationship.kind, "TimelineCache") {
            continue;
        }
        let Some(part) = relationship
            .resolved_part
            .as_ref()
            .filter(|part| parts.contains_key(*part))
        else {
            continue;
        };
        if !seen.insert(part.clone()) {
            continue;
        }
        let document = Document::parse(xml_part(parts, part)?)
            .map_err(|error| format!("{part} XML: {error}"))?;
        let root = document.root_element();
        if local_name(root) != "timelineCacheDefinition" {
            return Err(format!("{part} is not a timelineCacheDefinition"));
        }
        let state_node = direct_child(root, "state");
        let pivot_cache_id = state_node.and_then(|state| u32_attribute(state, "pivotCacheId"));
        let state = TimelineStateModel {
            single_range_filter_state: state_node
                .and_then(|state| bool_attribute(state, "singleRangeFilterState")),
            minimal_refresh_version: state_node
                .and_then(|state| u32_attribute(state, "minimalRefreshVersion")),
            last_refresh_version: state_node
                .and_then(|state| u32_attribute(state, "lastRefreshVersion")),
            pivot_cache_id,
            pivot_cache_part: pivot_cache_part_for_state(pivot_cache_id, pivot_caches),
            filter_type: state_node
                .and_then(|state| state.attribute("filterType"))
                .map(str::to_string),
            filter_id: state_node.and_then(|state| u32_attribute(state, "filterId")),
            filter_tab_id: state_node.and_then(|state| u32_attribute(state, "filterTabId")),
            filter_pivot_name: state_node
                .and_then(|state| state.attribute("filterPivotName"))
                .map(str::to_string),
            selection: state_node.and_then(|state| range_model(direct_child(state, "selection"))),
            bounds: state_node.and_then(|state| range_model(direct_child(state, "bounds"))),
            has_moving_period_state: state_node
                .is_some_and(|state| direct_child(state, "movingPeriodState").is_some()),
            has_timeline_pivot_filter: direct_child(root, "timelinePivotFilter").is_some(),
        };
        let mut connections = Vec::new();
        if let Some(container) = direct_child(root, "pivotTables") {
            for connection in direct_children(container, "pivotTable") {
                let (Some(tab_id), Some(name)) = (
                    u32_attribute(connection, "tabId"),
                    connection.attribute("name"),
                ) else {
                    continue;
                };
                let pivot = pivot_tables
                    .iter()
                    .find(|pivot| pivot.sheet_id == tab_id && pivot.name == name);
                connections.push(TimelineConnectionModel {
                    tab_id,
                    name: name.to_string(),
                    sheet: pivot.map(|pivot| pivot.sheet.clone()),
                    pivot_table_part: pivot.map(|pivot| pivot.part.clone()),
                    pivot_cache_part: pivot.and_then(|pivot| pivot.cache_part.clone()),
                    exists: pivot.is_some() || tab_id == u32::MAX,
                });
            }
        }
        result.push(TimelineCacheModel {
            part: part.clone(),
            relationship_id,
            name: root.attribute("name").map(str::to_string),
            uid: uid_attribute(root),
            source_name: root.attribute("sourceName").map(str::to_string),
            state,
            connections,
            view_parts: Vec::new(),
        });
    }
    result.sort_by(|left, right| left.part.cmp(&right.part));
    Ok(result)
}

fn parse_timeline_views(
    parts: &BTreeMap<String, Vec<u8>>,
    sheets: &[SheetInfo],
    caches: &[TimelineCacheModel],
) -> Result<Vec<TimelineViewModel>, String> {
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    for sheet in sheets {
        let sheet_document = Document::parse(xml_part(parts, &sheet.part)?)
            .map_err(|error| format!("{} XML: {error}", sheet.part))?;
        let relationships = parse_relationships(parts, &sheet.part)?;
        for reference in sheet_document
            .descendants()
            .filter(|node| node.is_element() && local_name(*node) == "timelineRef")
        {
            let Some(relationship_id) = relationship_id(reference) else {
                continue;
            };
            let Some(relationship) = relationships.get(&relationship_id) else {
                continue;
            };
            if !relationship_kind_is(&relationship.kind, "Timeline") {
                continue;
            }
            let Some(part) = relationship
                .resolved_part
                .as_ref()
                .filter(|part| parts.contains_key(*part))
            else {
                continue;
            };
            if !seen.insert((sheet.id, part.clone())) {
                continue;
            }
            let document = Document::parse(xml_part(parts, part)?)
                .map_err(|error| format!("{part} XML: {error}"))?;
            let root = document.root_element();
            if local_name(root) != "timelines" {
                return Err(format!("{part} is not a timelines part"));
            }
            for timeline in direct_children(root, "timeline") {
                let cache_name = timeline.attribute("cache").map(str::to_string);
                let cache_part = cache_name.as_deref().and_then(|name| {
                    caches
                        .iter()
                        .find(|cache| cache.name.as_deref() == Some(name))
                        .or_else(|| {
                            caches.iter().find(|cache| {
                                cache
                                    .name
                                    .as_deref()
                                    .is_some_and(|candidate| candidate.eq_ignore_ascii_case(name))
                            })
                        })
                        .map(|cache| cache.part.clone())
                });
                result.push(TimelineViewModel {
                    part: part.clone(),
                    relationship_id: relationship.id.clone(),
                    sheet: sheet.name.clone(),
                    sheet_id: sheet.id,
                    sheet_part: sheet.part.clone(),
                    name: timeline.attribute("name").map(str::to_string),
                    uid: uid_attribute(timeline),
                    cache: cache_name,
                    cache_part,
                    caption: timeline.attribute("caption").map(str::to_string),
                    show_header: bool_attribute(timeline, "showHeader"),
                    show_selection_label: bool_attribute(timeline, "showSelectionLabel"),
                    show_time_level: bool_attribute(timeline, "showTimeLevel"),
                    show_horizontal_scrollbar: bool_attribute(timeline, "showHorizontalScrollbar"),
                    level: u32_attribute(timeline, "level"),
                    selection_level: u32_attribute(timeline, "selectionLevel"),
                    scroll_position: timeline.attribute("scrollPosition").map(str::to_string),
                    style: timeline.attribute("style").map(str::to_string),
                });
            }
        }
    }
    result.sort_by(|left, right| {
        left.sheet_id
            .cmp(&right.sheet_id)
            .then_with(|| left.part.cmp(&right.part))
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(result)
}

/// Resolves Timeline caches, views, PivotCaches, and PivotTables through the native OPC graph.
pub(crate) fn parse_timeline_model(
    parts: &BTreeMap<String, Vec<u8>>,
) -> Result<TimelineWorkbookModel, String> {
    let workbook_part = office_document_part(parts)?;
    let workbook_xml = xml_part(parts, &workbook_part)?;
    let workbook_document =
        Document::parse(workbook_xml).map_err(|error| format!("{workbook_part} XML: {error}"))?;
    let workbook = workbook_document.root_element();
    let workbook_relationships = parse_relationships(parts, &workbook_part)?;
    let sheets = sheets(parts, workbook, &workbook_relationships);
    let pivot_caches = parse_pivot_caches(parts, workbook, &workbook_relationships)?;
    let pivot_tables = parse_pivot_tables(parts, &sheets)?;
    let mut caches = parse_timeline_caches(
        parts,
        workbook,
        &workbook_relationships,
        &pivot_caches,
        &pivot_tables,
    )?;
    let views = parse_timeline_views(parts, &sheets, &caches)?;
    for cache in &mut caches {
        cache.view_parts = views
            .iter()
            .filter(|view| view.cache_part.as_deref() == Some(cache.part.as_str()))
            .map(|view| view.part.clone())
            .collect();
        cache.view_parts.sort();
        cache.view_parts.dedup();
    }
    Ok(TimelineWorkbookModel {
        workbook_part,
        date_1904: workbook
            .descendants()
            .find(|node| node.is_element() && local_name(*node) == "workbookPr")
            .and_then(|node| bool_attribute(node, "date1904"))
            .unwrap_or(false),
        timeline_cache_pivot_cache_ids: timeline_cache_pivot_cache_ids(workbook),
        pivot_caches,
        pivot_tables,
        caches,
        views,
    })
}

fn start_tag_range(xml: &str, element_start: usize) -> Result<Range<usize>, String> {
    let bytes = xml.as_bytes();
    if bytes.get(element_start) != Some(&b'<') {
        return Err("element does not start with '<'".to_string());
    }
    let mut cursor = element_start;
    let mut quote = None;
    while cursor < bytes.len() {
        let current = bytes[cursor] as char;
        if let Some(open) = quote {
            if current == open {
                quote = None;
            }
        } else if matches!(current, '\'' | '"') {
            quote = Some(current);
        } else if current == '>' {
            return Ok(element_start..cursor + 1);
        }
        cursor += 1;
    }
    Err("XML start tag is not closed".to_string())
}

fn scan_start_tag_attributes(tag: &str) -> Result<Vec<AttributeSpan>, String> {
    let bytes = tag.as_bytes();
    if bytes.first() != Some(&b'<') || bytes.last() != Some(&b'>') {
        return Err("invalid XML start tag".to_string());
    }
    let mut cursor = 1usize;
    while cursor < bytes.len()
        && !bytes[cursor].is_ascii_whitespace()
        && !matches!(bytes[cursor], b'/' | b'>')
    {
        cursor += 1;
    }
    let mut attributes = Vec::new();
    loop {
        let whitespace_start = cursor;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= bytes.len() || matches!(bytes[cursor], b'/' | b'>') {
            break;
        }
        let name_start = cursor;
        while cursor < bytes.len()
            && !bytes[cursor].is_ascii_whitespace()
            && !matches!(bytes[cursor], b'=' | b'/' | b'>')
        {
            cursor += 1;
        }
        let name_end = cursor;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&b'=') {
            return Err("malformed XML attribute".to_string());
        }
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let quote = match bytes.get(cursor) {
            Some(b'\'') => b'\'',
            Some(b'"') => b'"',
            _ => return Err("XML attribute value is not quoted".to_string()),
        };
        cursor += 1;
        let value_start = cursor;
        while cursor < bytes.len() && bytes[cursor] != quote {
            cursor += 1;
        }
        if cursor >= bytes.len() {
            return Err("unterminated XML attribute value".to_string());
        }
        let value_end = cursor;
        cursor += 1;
        attributes.push(AttributeSpan {
            name: tag[name_start..name_end].to_string(),
            value_range: value_start..value_end,
            full_range: whitespace_start..cursor,
        });
    }
    Ok(attributes)
}

fn xml_escape_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn patch_element_attributes(
    xml: &str,
    element_start: usize,
    edits: &[(String, Option<String>)],
) -> Result<String, String> {
    if edits.is_empty() {
        return Ok(xml.to_string());
    }
    let tag_range = start_tag_range(xml, element_start)?;
    let tag = &xml[tag_range.clone()];
    let attributes = scan_start_tag_attributes(tag)?;
    let mut replacements: Vec<(Range<usize>, String)> = Vec::new();
    let mut inserts = Vec::new();
    for (name, value) in edits {
        if let Some(attribute) = attributes.iter().find(|attribute| attribute.name == *name) {
            let range = if value.is_some() {
                attribute.value_range.clone()
            } else {
                attribute.full_range.clone()
            };
            replacements.push((
                tag_range.start + range.start..tag_range.start + range.end,
                value
                    .as_deref()
                    .map(xml_escape_attribute)
                    .unwrap_or_default(),
            ));
        } else if let Some(value) = value {
            inserts.push(format!(" {name}=\"{}\"", xml_escape_attribute(value)));
        }
    }
    replacements.sort_by(|left, right| right.0.start.cmp(&left.0.start));
    let mut result = xml.to_string();
    for (range, value) in replacements {
        result.replace_range(range, &value);
    }
    if !inserts.is_empty() {
        // Existing replacements do not affect the end of the original tag if they are applied
        // before it; recompute the same element's start tag in the updated string.
        let updated_range = start_tag_range(&result, element_start)?;
        let updated_tag = &result[updated_range.clone()];
        let insert_at = if updated_tag
            .as_bytes()
            .get(updated_tag.len().saturating_sub(2))
            == Some(&b'/')
        {
            updated_range.end - 2
        } else {
            updated_range.end - 1
        };
        result.insert_str(insert_at, &inserts.concat());
    }
    Ok(result)
}

fn qname_at(xml: &str, element_start: usize) -> Result<&str, String> {
    let bytes = xml.as_bytes();
    if bytes.get(element_start) != Some(&b'<') {
        return Err("element does not start with '<'".to_string());
    }
    let start = element_start + 1;
    let mut end = start;
    while end < bytes.len()
        && !bytes[end].is_ascii_whitespace()
        && !matches!(bytes[end], b'/' | b'>')
    {
        end += 1;
    }
    Ok(&xml[start..end])
}

fn sibling_qname(xml: &str, parent_start: usize, local: &str) -> Result<String, String> {
    let parent = qname_at(xml, parent_start)?;
    Ok(match parent.rsplit_once(':') {
        Some((prefix, _)) => format!("{prefix}:{local}"),
        None => local.to_string(),
    })
}

fn validate_level(value: u32, name: &str) -> Result<(), String> {
    if value <= 3 {
        Ok(())
    } else {
        Err(format!("Timeline {name} must be 0 (year) through 3 (day)"))
    }
}

fn validate_text(value: &str, name: &str) -> Result<(), String> {
    if value.is_empty() {
        Err(format!("Timeline {name} cannot be empty"))
    } else {
        Ok(())
    }
}

fn is_leap_year(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn validate_date_time(value: &str) -> Result<(), String> {
    let Some((date, time)) = value.split_once('T') else {
        return Err(format!("Timeline dateTime {value:?} has no 'T' separator"));
    };
    let date_parts: Vec<&str> = date.trim_start_matches('-').split('-').collect();
    if date_parts.len() != 3 || date_parts[0].len() < 4 {
        return Err(format!("Timeline dateTime {value:?} has an invalid date"));
    }
    let sign = if date.starts_with('-') { -1i64 } else { 1 };
    let year = date_parts[0]
        .parse::<i64>()
        .map_err(|_| format!("Timeline dateTime {value:?} has an invalid year"))?
        * sign;
    let month = date_parts[1]
        .parse::<u32>()
        .map_err(|_| format!("Timeline dateTime {value:?} has an invalid month"))?;
    let day = date_parts[2]
        .parse::<u32>()
        .map_err(|_| format!("Timeline dateTime {value:?} has an invalid day"))?;
    let max_day = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    };
    if day == 0 || day > max_day {
        return Err(format!(
            "Timeline dateTime {value:?} has an invalid calendar day"
        ));
    }
    let (clock, zone) = if let Some(clock) = time.strip_suffix('Z') {
        (clock, Some("Z"))
    } else if let Some(index) = time
        .char_indices()
        .skip(1)
        .find_map(|(index, value)| matches!(value, '+' | '-').then_some(index))
    {
        (&time[..index], Some(&time[index..]))
    } else {
        (time, None)
    };
    let clock_parts: Vec<&str> = clock.split(':').collect();
    if clock_parts.len() != 3 {
        return Err(format!("Timeline dateTime {value:?} has an invalid time"));
    }
    let hour = clock_parts[0]
        .parse::<u32>()
        .map_err(|_| format!("Timeline dateTime {value:?} has an invalid hour"))?;
    let minute = clock_parts[1]
        .parse::<u32>()
        .map_err(|_| format!("Timeline dateTime {value:?} has an invalid minute"))?;
    let seconds = clock_parts[2]
        .split_once('.')
        .map(|(seconds, fraction)| {
            (!fraction.is_empty() && fraction.bytes().all(|value| value.is_ascii_digit()))
                .then_some(seconds)
        })
        .unwrap_or(Some(clock_parts[2]))
        .ok_or_else(|| format!("Timeline dateTime {value:?} has an invalid fraction"))?
        .parse::<u32>()
        .map_err(|_| format!("Timeline dateTime {value:?} has invalid seconds"))?;
    if hour > 23 || minute > 59 || seconds > 59 {
        return Err(format!(
            "Timeline dateTime {value:?} is outside the clock range"
        ));
    }
    if let Some(zone) = zone.filter(|zone| *zone != "Z") {
        let zone = &zone[1..];
        let parts: Vec<&str> = zone.split(':').collect();
        let valid = parts.len() == 2
            && parts[0].parse::<u32>().is_ok_and(|hour| hour <= 14)
            && parts[1].parse::<u32>().is_ok_and(|minute| minute <= 59);
        if !valid {
            return Err(format!(
                "Timeline dateTime {value:?} has an invalid timezone"
            ));
        }
    }
    Ok(())
}

fn validate_range(value: &TimelineRange) -> Result<(), String> {
    validate_date_time(&value.start_date)?;
    validate_date_time(&value.end_date)?;
    if value.start_date > value.end_date {
        return Err("Timeline range startDate must not be after endDate".to_string());
    }
    Ok(())
}

fn push_string_edit(
    edits: &mut Vec<(String, Option<String>)>,
    node: Node<'_, '_>,
    name: &str,
    patch: &PatchField<String>,
    nullable: bool,
) -> Result<(), String> {
    match patch {
        PatchField::Missing => {}
        PatchField::Null if nullable => {
            if node.attribute(name).is_some() {
                edits.push((name.to_string(), None));
            }
        }
        PatchField::Null => return Err(format!("Timeline {name} is required and cannot be null")),
        PatchField::Value(value) => {
            validate_text(value, name)?;
            if node.attribute(name) != Some(value.as_str()) {
                edits.push((name.to_string(), Some(value.clone())));
            }
        }
    }
    Ok(())
}

fn push_bool_edit(
    edits: &mut Vec<(String, Option<String>)>,
    node: Node<'_, '_>,
    name: &str,
    patch: &PatchField<bool>,
) {
    match patch {
        PatchField::Missing => {}
        PatchField::Null => {
            if node.attribute(name).is_some() {
                edits.push((name.to_string(), None));
            }
        }
        PatchField::Value(value) => {
            if bool_attribute(node, name) != Some(*value) {
                edits.push((
                    name.to_string(),
                    Some(if *value { "1" } else { "0" }.to_string()),
                ));
            }
        }
    }
}

/// Applies a differential patch to one named `timeline` in a worksheet Timelines part.
pub(crate) fn apply_timeline_view_patch(
    timelines_xml: &str,
    timeline_name: &str,
    patch: &TimelineViewPatch,
) -> Result<String, String> {
    let document =
        Document::parse(timelines_xml).map_err(|error| format!("Timelines XML: {error}"))?;
    let root = document.root_element();
    if local_name(root) != "timelines" {
        return Err("root is not timelines".to_string());
    }
    let mut matches = direct_children(root, "timeline")
        .filter(|timeline| timeline.attribute("name") == Some(timeline_name));
    let timeline = matches
        .next()
        .ok_or_else(|| format!("Timeline view {timeline_name:?} does not exist"))?;
    if matches.next().is_some() {
        return Err(format!("Timeline view name {timeline_name:?} is ambiguous"));
    }
    let mut edits = Vec::new();
    push_string_edit(&mut edits, timeline, "caption", &patch.caption, true)?;
    push_string_edit(&mut edits, timeline, "style", &patch.style, true)?;
    if let PatchField::Value(value) = &patch.scroll_position {
        validate_date_time(value)?;
    }
    push_string_edit(
        &mut edits,
        timeline,
        "scrollPosition",
        &patch.scroll_position,
        true,
    )?;
    for (name, value) in [
        ("level", &patch.level),
        ("selectionLevel", &patch.selection_level),
    ] {
        match value {
            PatchField::Missing => {}
            PatchField::Null => {
                return Err(format!("Timeline {name} is required and cannot be null"));
            }
            PatchField::Value(value) => {
                validate_level(*value, name)?;
                if u32_attribute(timeline, name) != Some(*value) {
                    edits.push((name.to_string(), Some(value.to_string())));
                }
            }
        }
    }
    push_bool_edit(&mut edits, timeline, "showHeader", &patch.show_header);
    push_bool_edit(
        &mut edits,
        timeline,
        "showSelectionLabel",
        &patch.show_selection_label,
    );
    push_bool_edit(
        &mut edits,
        timeline,
        "showTimeLevel",
        &patch.show_time_level,
    );
    push_bool_edit(
        &mut edits,
        timeline,
        "showHorizontalScrollbar",
        &patch.show_horizontal_scrollbar,
    );
    patch_element_attributes(timelines_xml, timeline.range().start, &edits)
}

fn state_start(xml: &str) -> Result<(usize, bool, bool), String> {
    let document = Document::parse(xml).map_err(|error| format!("Timeline cache XML: {error}"))?;
    let root = document.root_element();
    if local_name(root) != "timelineCacheDefinition" {
        return Err("root is not timelineCacheDefinition".to_string());
    }
    let state = direct_child(root, "state").ok_or("Timeline cache has no state element")?;
    Ok((
        state.range().start,
        direct_child(state, "movingPeriodState").is_some(),
        direct_child(root, "timelinePivotFilter").is_some(),
    ))
}

fn remove_direct_child(xml: &str, parent_name: &str, child_name: &str) -> Result<String, String> {
    let document = Document::parse(xml).map_err(|error| format!("Timeline XML: {error}"))?;
    let parent = document
        .descendants()
        .find(|node| node.is_element() && local_name(*node) == parent_name)
        .ok_or_else(|| format!("Timeline XML has no {parent_name}"))?;
    let Some(child) = direct_child(parent, child_name) else {
        return Ok(xml.to_string());
    };
    let mut result = xml.to_string();
    result.replace_range(child.range(), "");
    Ok(result)
}

fn range_child_info(
    xml: &str,
    child_name: &str,
) -> Result<(usize, Option<(usize, TimelineRange)>), String> {
    let document = Document::parse(xml).map_err(|error| format!("Timeline cache XML: {error}"))?;
    let root = document.root_element();
    let state = direct_child(root, "state").ok_or("Timeline cache has no state element")?;
    let child = direct_child(state, child_name);
    Ok((
        state.range().start,
        child.map(|child| {
            (
                child.range().start,
                TimelineRange {
                    start_date: child.attribute("startDate").unwrap_or("").to_string(),
                    end_date: child.attribute("endDate").unwrap_or("").to_string(),
                },
            )
        }),
    ))
}

fn insert_range_child(
    xml: &str,
    child_name: &str,
    value: &TimelineRange,
) -> Result<String, String> {
    let document = Document::parse(xml).map_err(|error| format!("Timeline cache XML: {error}"))?;
    let root = document.root_element();
    let state = direct_child(root, "state").ok_or("Timeline cache has no state element")?;
    let qname = sibling_qname(xml, state.range().start, child_name)?;
    let markup = format!(
        "<{qname} startDate=\"{}\" endDate=\"{}\"/>",
        xml_escape_attribute(&value.start_date),
        xml_escape_attribute(&value.end_date)
    );
    let insert_at = if child_name == "selection" {
        direct_child(state, "bounds")
            .map(|node| node.range().start)
            .or_else(|| {
                state
                    .children()
                    .find(|node| node.is_element())
                    .map(|node| node.range().start)
            })
    } else {
        state
            .children()
            .find(|node| {
                node.is_element() && matches!(local_name(*node), "movingPeriodState" | "extLst")
            })
            .map(|node| node.range().start)
    }
    .unwrap_or_else(|| {
        let range = state.range();
        xml[range.clone()]
            .rfind("</")
            .map(|relative| range.start + relative)
            .unwrap_or(range.end)
    });
    let mut result = xml.to_string();
    result.insert_str(insert_at, &markup);
    Ok(result)
}

fn apply_range_field(
    xml: &str,
    child_name: &str,
    patch: &PatchField<TimelineRange>,
    required: bool,
) -> Result<String, String> {
    match patch {
        PatchField::Missing => Ok(xml.to_string()),
        PatchField::Null if required => Err(format!(
            "Timeline cache {child_name} is required and cannot be null"
        )),
        PatchField::Null => remove_direct_child(xml, "state", child_name),
        PatchField::Value(value) => {
            validate_range(value)?;
            let (_, child) = range_child_info(xml, child_name)?;
            let Some((start, existing)) = child else {
                return insert_range_child(xml, child_name, value);
            };
            if existing == *value {
                return Ok(xml.to_string());
            }
            let mut edits = Vec::new();
            if existing.start_date != value.start_date {
                edits.push(("startDate".to_string(), Some(value.start_date.clone())));
            }
            if existing.end_date != value.end_date {
                edits.push(("endDate".to_string(), Some(value.end_date.clone())));
            }
            patch_element_attributes(xml, start, &edits)
        }
    }
}

fn connection_key(connection: &TimelineConnectionTarget) -> (u32, &str) {
    (connection.tab_id, connection.name.as_str())
}

fn validate_connection_delta(delta: &TimelineConnectionDelta) -> Result<(), String> {
    let mut add = HashSet::new();
    let mut remove = HashSet::new();
    for connection in &delta.add {
        validate_text(&connection.name, "connection name")?;
        if !add.insert(connection_key(connection)) {
            return Err(format!(
                "duplicate Timeline connection add target {} / {}",
                connection.tab_id, connection.name
            ));
        }
    }
    for connection in &delta.remove {
        validate_text(&connection.name, "connection name")?;
        if !remove.insert(connection_key(connection)) {
            return Err(format!(
                "duplicate Timeline connection remove target {} / {}",
                connection.tab_id, connection.name
            ));
        }
    }
    if let Some(target) = add.intersection(&remove).next() {
        return Err(format!(
            "Timeline connection {} / {} cannot be added and removed together",
            target.0, target.1
        ));
    }
    Ok(())
}

fn pivot_tables_is_safely_removable(container: Node<'_, '_>) -> bool {
    container.attributes().count() == 0
        && container.children().all(|node| {
            (node.is_element() && local_name(node) == "pivotTable")
                || (node.is_text() && node.text().unwrap_or("").trim().is_empty())
        })
}

fn append_connections(xml: &str, additions: &[TimelineConnectionTarget]) -> Result<String, String> {
    if additions.is_empty() {
        return Ok(xml.to_string());
    }
    let document = Document::parse(xml).map_err(|error| format!("Timeline cache XML: {error}"))?;
    let root = document.root_element();
    let markup_for = |qname: &str, connection: &TimelineConnectionTarget| {
        format!(
            "<{qname} tabId=\"{}\" name=\"{}\"/>",
            connection.tab_id,
            xml_escape_attribute(&connection.name)
        )
    };
    if let Some(container) = direct_child(root, "pivotTables") {
        let qname = sibling_qname(xml, container.range().start, "pivotTable")?;
        let markup: String = additions
            .iter()
            .map(|connection| markup_for(&qname, connection))
            .collect();
        let tag_range = start_tag_range(xml, container.range().start)?;
        let tag = &xml[tag_range.clone()];
        let mut result = xml.to_string();
        if tag.as_bytes().get(tag.len().saturating_sub(2)) == Some(&b'/') {
            let container_qname = qname_at(xml, container.range().start)?;
            result.replace_range(
                tag_range.end - 2..tag_range.end,
                &format!(">{markup}</{container_qname}>"),
            );
        } else {
            let range = container.range();
            let insert_at = xml[range.clone()]
                .rfind("</")
                .map(|relative| range.start + relative)
                .ok_or("Timeline pivotTables container has no closing tag")?;
            result.insert_str(insert_at, &markup);
        }
        return Ok(result);
    }
    let container_qname = sibling_qname(xml, root.range().start, "pivotTables")?;
    let child_qname = sibling_qname(xml, root.range().start, "pivotTable")?;
    let children: String = additions
        .iter()
        .map(|connection| markup_for(&child_qname, connection))
        .collect();
    let markup = format!("<{container_qname}>{children}</{container_qname}>");
    let insert_at = direct_child(root, "state")
        .map(|state| state.range().start)
        .ok_or("Timeline cache has no state element")?;
    let mut result = xml.to_string();
    result.insert_str(insert_at, &markup);
    Ok(result)
}

fn apply_connection_delta(xml: &str, delta: &TimelineConnectionDelta) -> Result<String, String> {
    validate_connection_delta(delta)?;
    if delta.add.is_empty() && delta.remove.is_empty() {
        return Ok(xml.to_string());
    }
    let document = Document::parse(xml).map_err(|error| format!("Timeline cache XML: {error}"))?;
    let root = document.root_element();
    let container = direct_child(root, "pivotTables");
    let mut existing = HashSet::new();
    let mut remove_ranges = Vec::new();
    if let Some(container) = container {
        for node in direct_children(container, "pivotTable") {
            let Some(tab_id) = u32_attribute(node, "tabId") else {
                continue;
            };
            let Some(name) = node.attribute("name") else {
                continue;
            };
            existing.insert((tab_id, name.to_string()));
            if delta
                .remove
                .iter()
                .any(|target| target.tab_id == tab_id && target.name == name)
            {
                remove_ranges.push(node.range());
            }
        }
    }
    let additions: Vec<TimelineConnectionTarget> = delta
        .add
        .iter()
        .filter(|target| !existing.contains(&(target.tab_id, target.name.clone())))
        .cloned()
        .collect();
    let remaining = existing.len().saturating_sub(remove_ranges.len()) + additions.len();
    if remaining == 0 {
        let Some(container) = container else {
            return Ok(xml.to_string());
        };
        if !pivot_tables_is_safely_removable(container) {
            return Err(
                "cannot remove the last Timeline connection because pivotTables has opaque content"
                    .to_string(),
            );
        }
        let mut result = xml.to_string();
        result.replace_range(container.range(), "");
        return Ok(result);
    }
    remove_ranges.sort_by(|left, right| right.start.cmp(&left.start));
    let mut result = xml.to_string();
    for range in remove_ranges {
        result.replace_range(range, "");
    }
    append_connections(&result, &additions)
}

/// Applies a differential patch to a native `timelineCacheDefinition` part.
pub(crate) fn apply_timeline_cache_patch(
    cache_xml: &str,
    patch: &TimelineCachePatch,
) -> Result<String, String> {
    let (state_start, has_moving_period, has_pivot_filter) = state_start(cache_xml)?;
    // A native Excel `SetFilterDateRange` edit writes both the selection element and
    // filterType=dateBetween.  Clearing the range returns filterType to unknown.  Treat those
    // pairs as one operation even when callers only supplied the range.
    let effective_filter_type = match (&patch.filter_type, &patch.selection) {
        (PatchField::Missing, PatchField::Value(_)) => PatchField::Value("dateBetween".to_string()),
        (PatchField::Missing, PatchField::Null) => PatchField::Value("unknown".to_string()),
        (value, _) => value.clone(),
    };
    if !matches!(patch.selection, PatchField::Missing)
        && has_moving_period
        && !patch.clear_moving_period_state
    {
        return Err(
            "Timeline selection cannot be changed while movingPeriodState exists; set clearMovingPeriodState"
                .to_string(),
        );
    }
    if let PatchField::Value(filter_type) = &effective_filter_type {
        validate_text(filter_type, "filterType")?;
        if matches!(
            filter_type.as_str(),
            "dateBetween" | "dateEqual" | "unknown"
        ) && has_pivot_filter
            && !patch.clear_timeline_pivot_filter
        {
            return Err(
                "filterType requires clearing the existing timelinePivotFilter; set clearTimelinePivotFilter"
                    .to_string(),
            );
        }
    }
    let document =
        Document::parse(cache_xml).map_err(|error| format!("Timeline cache XML: {error}"))?;
    let root = document.root_element();
    let state = direct_child(root, "state").ok_or("Timeline cache has no state element")?;
    if let PatchField::Value(filter_type) = &effective_filter_type {
        let existing_filter_type = state.attribute("filterType");
        let supported_static_range = matches!(filter_type.as_str(), "dateBetween" | "unknown");
        if !supported_static_range
            && (existing_filter_type != Some(filter_type.as_str())
                || !matches!(patch.selection, PatchField::Missing)
                || patch.clear_timeline_pivot_filter)
        {
            return Err(format!(
                "editing Timeline filterType {filter_type:?} requires the unsupported relative/OLAP timelinePivotFilter model"
            ));
        }
        match (filter_type.as_str(), &patch.selection) {
            ("dateBetween", PatchField::Null) => {
                return Err("dateBetween requires a Timeline selection range".to_string());
            }
            ("dateBetween", PatchField::Missing) if direct_child(state, "selection").is_none() => {
                return Err("dateBetween requires a Timeline selection range".to_string());
            }
            ("unknown", PatchField::Value(_)) => {
                return Err("filterType unknown cannot retain a Timeline selection".to_string());
            }
            ("unknown", PatchField::Missing)
                if existing_filter_type != Some("unknown")
                    && direct_child(state, "selection").is_some() =>
            {
                return Err(
                    "clearing filterType requires selection:null in the same Timeline patch"
                        .to_string(),
                );
            }
            _ => {}
        }
    }
    let mut state_edits = Vec::new();
    push_string_edit(
        &mut state_edits,
        state,
        "filterType",
        &effective_filter_type,
        false,
    )?;
    push_bool_edit(
        &mut state_edits,
        state,
        "singleRangeFilterState",
        &patch.single_range_filter_state,
    );
    let mut result = patch_element_attributes(cache_xml, state_start, &state_edits)?;
    if patch.clear_moving_period_state {
        result = remove_direct_child(&result, "state", "movingPeriodState")?;
    }
    if patch.clear_timeline_pivot_filter {
        result = remove_direct_child(&result, "timelineCacheDefinition", "timelinePivotFilter")?;
    }
    result = apply_range_field(&result, "selection", &patch.selection, false)?;
    result = apply_range_field(&result, "bounds", &patch.bounds, true)?;
    if let Some(delta) = &patch.connections {
        result = apply_connection_delta(&result, delta)?;
    }
    Ok(result)
}

fn civil_days_from_unix_epoch(year: i64, month: u32, day: u32) -> i64 {
    // Howard Hinnant's proleptic-Gregorian civil calendar conversion.  The result is the number
    // of days since 1970-01-01 and avoids a heavyweight date dependency in the XLSX transport.
    let year = year - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let shifted_month = month as i64 + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn excel_date_serial(value: &str, date_1904: bool) -> Result<i64, String> {
    validate_date_time(value)?;
    let date = value
        .split_once('T')
        .map(|(date, _)| date)
        .ok_or("Timeline dateTime has no date")?;
    let negative = date.starts_with('-');
    let fields: Vec<&str> = date.trim_start_matches('-').split('-').collect();
    let mut year = fields[0]
        .parse::<i64>()
        .map_err(|_| format!("Timeline dateTime {value:?} has an invalid year"))?;
    if negative {
        year = -year;
    }
    let month = fields[1]
        .parse::<u32>()
        .map_err(|_| format!("Timeline dateTime {value:?} has an invalid month"))?;
    let day = fields[2]
        .parse::<u32>()
        .map_err(|_| format!("Timeline dateTime {value:?} has an invalid day"))?;
    let unix_days = civil_days_from_unix_epoch(year, month, day);
    // 1970-01-01 is 25569 in Excel's 1900 system (including Excel's fictitious 1900-02-29),
    // and 24107 in the 1904 system.
    Ok(unix_days + if date_1904 { 24_107 } else { 25_569 })
}

fn timeline_filter_candidate(node: Node<'_, '_>, field_index: u32) -> bool {
    u32_attribute(node, "fld") == Some(field_index)
        && (node
            .descendants()
            .any(|child| child.is_element() && local_name(child) == "pivotFilter")
            || node
                .attribute("type")
                .is_some_and(|kind| kind.starts_with("date")))
}

fn timeline_filter_start(xml: &str, field_index: u32) -> Result<Option<usize>, String> {
    let document = Document::parse(xml).map_err(|error| format!("PivotTable XML: {error}"))?;
    let root = document.root_element();
    if local_name(root) != "pivotTableDefinition" {
        return Err("root is not pivotTableDefinition".to_string());
    }
    let Some(filters) = direct_child(root, "filters") else {
        return Ok(None);
    };
    let matches: Vec<Node<'_, '_>> = direct_children(filters, "filter")
        .filter(|filter| timeline_filter_candidate(*filter, field_index))
        .collect();
    match matches.as_slice() {
        [] => Ok(None),
        [filter] => Ok(Some(filter.range().start)),
        _ => Err(format!(
            "PivotTable has multiple Timeline date filters for field {field_index}"
        )),
    }
}

fn descendant_start(
    xml: &str,
    ancestor_start: usize,
    local: &str,
    attribute: Option<(&str, &str)>,
) -> Result<usize, String> {
    let document = Document::parse(xml).map_err(|error| format!("PivotTable XML: {error}"))?;
    let ancestor = document
        .descendants()
        .find(|node| node.is_element() && node.range().start == ancestor_start)
        .ok_or("PivotTable edited element disappeared")?;
    ancestor
        .descendants()
        .find(|node| {
            node.is_element()
                && local_name(*node) == local
                && attribute.is_none_or(|(name, value)| node.attribute(name) == Some(value))
        })
        .map(|node| node.range().start)
        .ok_or_else(|| format!("Timeline PivotTable filter has no expected {local} element"))
}

fn patch_existing_timeline_filter(
    pivot_xml: &str,
    filter_start: usize,
    field_index: u32,
    source_name: &str,
    start_serial: i64,
    end_serial: i64,
) -> Result<String, String> {
    let document =
        Document::parse(pivot_xml).map_err(|error| format!("PivotTable XML: {error}"))?;
    let filter = document
        .descendants()
        .find(|node| node.is_element() && node.range().start == filter_start)
        .ok_or("Timeline PivotTable filter disappeared")?;
    let mut filter_edits = Vec::new();
    if u32_attribute(filter, "fld") != Some(field_index) {
        filter_edits.push(("fld".to_string(), Some(field_index.to_string())));
    }
    if filter.attribute("type") != Some("dateBetween") {
        filter_edits.push(("type".to_string(), Some("dateBetween".to_string())));
    }
    if filter.attribute("name") != Some(source_name) {
        filter_edits.push(("name".to_string(), Some(source_name.to_string())));
    }
    let mut result = patch_element_attributes(pivot_xml, filter_start, &filter_edits)?;

    // Re-resolve every descendant after an earlier start-tag edit because byte offsets can move.
    let filter_start = timeline_filter_start(&result, field_index)?
        .ok_or("Timeline PivotTable filter disappeared after attribute edit")?;
    let auto_filter_start = descendant_start(&result, filter_start, "autoFilter", None)?;
    let auto_document =
        Document::parse(&result).map_err(|error| format!("PivotTable XML: {error}"))?;
    let auto_filter = auto_document
        .descendants()
        .find(|node| node.is_element() && node.range().start == auto_filter_start)
        .unwrap();
    let auto_edits = (auto_filter.attribute("ref") != Some("A1"))
        .then(|| vec![("ref".to_string(), Some("A1".to_string()))])
        .unwrap_or_default();
    result = patch_element_attributes(&result, auto_filter_start, &auto_edits)?;

    let filter_start = timeline_filter_start(&result, field_index)?.unwrap();
    let column_start = descendant_start(&result, filter_start, "filterColumn", None)?;
    let column_document =
        Document::parse(&result).map_err(|error| format!("PivotTable XML: {error}"))?;
    let column = column_document
        .descendants()
        .find(|node| node.is_element() && node.range().start == column_start)
        .unwrap();
    let column_edits = (u32_attribute(column, "colId") != Some(0))
        .then(|| vec![("colId".to_string(), Some("0".to_string()))])
        .unwrap_or_default();
    result = patch_element_attributes(&result, column_start, &column_edits)?;

    let filter_start = timeline_filter_start(&result, field_index)?.unwrap();
    let customs_start = descendant_start(&result, filter_start, "customFilters", None)?;
    let customs_document =
        Document::parse(&result).map_err(|error| format!("PivotTable XML: {error}"))?;
    let customs = customs_document
        .descendants()
        .find(|node| node.is_element() && node.range().start == customs_start)
        .unwrap();
    let customs_edits = (bool_attribute(customs, "and") != Some(true))
        .then(|| vec![("and".to_string(), Some("1".to_string()))])
        .unwrap_or_default();
    result = patch_element_attributes(&result, customs_start, &customs_edits)?;

    for (operator, serial) in [
        ("greaterThanOrEqual", start_serial),
        ("lessThanOrEqual", end_serial),
    ] {
        let filter_start = timeline_filter_start(&result, field_index)?.unwrap();
        let custom_start = descendant_start(
            &result,
            filter_start,
            "customFilter",
            Some(("operator", operator)),
        )?;
        let custom_document =
            Document::parse(&result).map_err(|error| format!("PivotTable XML: {error}"))?;
        let custom = custom_document
            .descendants()
            .find(|node| node.is_element() && node.range().start == custom_start)
            .unwrap();
        let expected = serial.to_string();
        let edits = (custom.attribute("val") != Some(expected.as_str()))
            .then(|| vec![("val".to_string(), Some(expected))])
            .unwrap_or_default();
        result = patch_element_attributes(&result, custom_start, &edits)?;
    }

    let filter_start = timeline_filter_start(&result, field_index)?.unwrap();
    let pivot_filter_start = descendant_start(&result, filter_start, "pivotFilter", None)?;
    let marker_document =
        Document::parse(&result).map_err(|error| format!("PivotTable XML: {error}"))?;
    let marker = marker_document
        .descendants()
        .find(|node| node.is_element() && node.range().start == pivot_filter_start)
        .unwrap();
    let marker_edits = (bool_attribute(marker, "useWholeDay") != Some(true))
        .then(|| vec![("useWholeDay".to_string(), Some("1".to_string()))])
        .unwrap_or_default();
    patch_element_attributes(&result, pivot_filter_start, &marker_edits)
}

fn filters_safely_removable(filters: Node<'_, '_>) -> bool {
    filters
        .attributes()
        .all(|attribute| attribute.name() == "count")
        && filters.children().all(|node| {
            (node.is_element() && local_name(node) == "filter")
                || (node.is_text() && node.text().unwrap_or("").trim().is_empty())
        })
}

fn remove_timeline_filter(pivot_xml: &str, field_index: u32) -> Result<String, String> {
    let document =
        Document::parse(pivot_xml).map_err(|error| format!("PivotTable XML: {error}"))?;
    let root = document.root_element();
    let Some(filters) = direct_child(root, "filters") else {
        return Ok(pivot_xml.to_string());
    };
    let matches: Vec<Node<'_, '_>> = direct_children(filters, "filter")
        .filter(|filter| timeline_filter_candidate(*filter, field_index))
        .collect();
    if matches.is_empty() {
        return Ok(pivot_xml.to_string());
    }
    if matches.len() > 1 {
        return Err(format!(
            "PivotTable has multiple Timeline date filters for field {field_index}"
        ));
    }
    let target = matches[0];
    let known_count = direct_children(filters, "filter").count();
    if known_count == 1 {
        if !filters_safely_removable(filters) {
            return Err(
                "cannot remove the last Timeline filter because filters has opaque content"
                    .to_string(),
            );
        }
        let mut result = pivot_xml.to_string();
        result.replace_range(filters.range(), "");
        return Ok(result);
    }
    let mut result = pivot_xml.to_string();
    result.replace_range(target.range(), "");
    let updated = Document::parse(&result).map_err(|error| format!("PivotTable XML: {error}"))?;
    let updated_filters = direct_child(updated.root_element(), "filters").unwrap();
    let actual = direct_children(updated_filters, "filter").count();
    if u32_attribute(updated_filters, "count") != Some(actual as u32) {
        result = patch_element_attributes(
            &result,
            updated_filters.range().start,
            &[("count".to_string(), Some(actual.to_string()))],
        )?;
    }
    Ok(result)
}

fn next_pivot_filter_id(root: Node<'_, '_>) -> u32 {
    root.descendants()
        .filter(|node| node.is_element() && local_name(*node) == "filter")
        .filter_map(|node| u32_attribute(node, "id"))
        .max()
        .unwrap_or(0)
        .saturating_add(1)
}

fn insert_timeline_filter(
    pivot_xml: &str,
    field_index: u32,
    source_name: &str,
    start_serial: i64,
    end_serial: i64,
) -> Result<String, String> {
    let document =
        Document::parse(pivot_xml).map_err(|error| format!("PivotTable XML: {error}"))?;
    let root = document.root_element();
    let prefix = qname_at(pivot_xml, root.range().start)?
        .rsplit_once(':')
        .map(|(prefix, _)| format!("{prefix}:"))
        .unwrap_or_default();
    let id = next_pivot_filter_id(root);
    let q = |local: &str| format!("{prefix}{local}");
    let markup = format!(
        "<{} fld=\"{}\" type=\"dateBetween\" evalOrder=\"-1\" id=\"{}\" name=\"{}\"><{} ref=\"A1\"><{} colId=\"0\"><{} and=\"1\"><{} operator=\"greaterThanOrEqual\" val=\"{}\"/><{} operator=\"lessThanOrEqual\" val=\"{}\"/></{}></{}></{}><{}><{} uri=\"{{0605FD5F-26C8-4aeb-8148-2DB25E43C511}}\" xmlns:x15=\"http://schemas.microsoft.com/office/spreadsheetml/2010/11/main\"><x15:pivotFilter useWholeDay=\"1\"/></{}></{}></{}>",
        q("filter"),
        field_index,
        id,
        xml_escape_attribute(source_name),
        q("autoFilter"),
        q("filterColumn"),
        q("customFilters"),
        q("customFilter"),
        start_serial,
        q("customFilter"),
        end_serial,
        q("customFilters"),
        q("filterColumn"),
        q("autoFilter"),
        q("extLst"),
        q("ext"),
        q("ext"),
        q("extLst"),
        q("filter"),
    );
    if let Some(filters) = direct_child(root, "filters") {
        let range = filters.range();
        let tag_range = start_tag_range(pivot_xml, range.start)?;
        let tag = &pivot_xml[tag_range.clone()];
        let mut result = pivot_xml.to_string();
        if tag.as_bytes().get(tag.len().saturating_sub(2)) == Some(&b'/') {
            let filters_qname = qname_at(pivot_xml, range.start)?;
            result.replace_range(
                tag_range.end - 2..tag_range.end,
                &format!(">{markup}</{filters_qname}>"),
            );
        } else {
            let insert_at = pivot_xml[range.clone()]
                .rfind("</")
                .map(|relative| range.start + relative)
                .ok_or("PivotTable filters container has no closing tag")?;
            result.insert_str(insert_at, &markup);
        }
        let updated =
            Document::parse(&result).map_err(|error| format!("PivotTable XML: {error}"))?;
        let updated_filters = direct_child(updated.root_element(), "filters").unwrap();
        let count = direct_children(updated_filters, "filter").count();
        return patch_element_attributes(
            &result,
            updated_filters.range().start,
            &[("count".to_string(), Some(count.to_string()))],
        );
    }
    let filters_qname = q("filters");
    let container = format!("<{filters_qname} count=\"1\">{markup}</{filters_qname}>");
    let insert_at = root
        .children()
        .find(|node| {
            node.is_element()
                && matches!(
                    local_name(*node),
                    "rowHierarchiesUsage" | "colHierarchiesUsage" | "extLst"
                )
        })
        .map(|node| node.range().start)
        .unwrap_or_else(|| {
            let range = root.range();
            pivot_xml[range.clone()]
                .rfind("</")
                .map(|relative| range.start + relative)
                .unwrap_or(range.end)
        });
    let mut result = pivot_xml.to_string();
    result.insert_str(insert_at, &container);
    Ok(result)
}

/// Applies/removes the native PivotTable date filter paired with a Timeline selection.
///
/// Existing Excel-generated filters are patched in place so their ids, opaque attributes, and
/// extension payloads survive.  `None` removes only the Timeline-owned date filter for `fld`.
pub(crate) fn apply_pivot_table_timeline_filter(
    pivot_xml: &str,
    field_index: u32,
    source_name: &str,
    selection: Option<&TimelineRange>,
    date_1904: bool,
) -> Result<String, String> {
    validate_text(source_name, "sourceName")?;
    let Some(selection) = selection else {
        return remove_timeline_filter(pivot_xml, field_index);
    };
    validate_range(selection)?;
    let start_serial = excel_date_serial(&selection.start_date, date_1904)?;
    let end_serial = excel_date_serial(&selection.end_date, date_1904)?;
    if let Some(filter_start) = timeline_filter_start(pivot_xml, field_index)? {
        patch_existing_timeline_filter(
            pivot_xml,
            filter_start,
            field_index,
            source_name,
            start_serial,
            end_serial,
        )
    } else {
        insert_timeline_filter(
            pivot_xml,
            field_index,
            source_name,
            start_serial,
            end_serial,
        )
    }
}

fn pivot_cache_field_index(
    parts: &BTreeMap<String, Vec<u8>>,
    pivot_cache_part: &str,
    source_name: &str,
) -> Result<u32, String> {
    let document = Document::parse(xml_part(parts, pivot_cache_part)?)
        .map_err(|error| format!("{pivot_cache_part} XML: {error}"))?;
    let root = document.root_element();
    let fields = direct_child(root, "cacheFields")
        .ok_or_else(|| format!("PivotCache {pivot_cache_part} has no cacheFields"))?;
    direct_children(fields, "cacheField")
        .position(|field| field.attribute("name") == Some(source_name))
        .map(|index| index as u32)
        .ok_or_else(|| format!("PivotCache {pivot_cache_part} has no field named {source_name:?}"))
}

fn cache_filter_snapshot(
    cache_xml: &str,
) -> Result<
    (
        Option<String>,
        Option<TimelineRange>,
        Vec<TimelineConnectionTarget>,
    ),
    String,
> {
    let document =
        Document::parse(cache_xml).map_err(|error| format!("Timeline cache XML: {error}"))?;
    let root = document.root_element();
    let state = direct_child(root, "state").ok_or("Timeline cache has no state")?;
    let filter_type = state.attribute("filterType").map(str::to_string);
    let selection = range_model(direct_child(state, "selection"));
    let connections = direct_child(root, "pivotTables")
        .into_iter()
        .flat_map(|container| direct_children(container, "pivotTable"))
        .filter_map(|node| {
            Some(TimelineConnectionTarget {
                tab_id: u32_attribute(node, "tabId")?,
                name: node.attribute("name")?.to_string(),
            })
        })
        .collect();
    Ok((filter_type, selection, connections))
}

fn validate_edit_request(
    model: &TimelineWorkbookModel,
    request: &TimelineEditRequest,
) -> Result<(), String> {
    if request.view.is_none() && request.cache.is_none() {
        return Err("Timeline edit request has no view or cache patch".to_string());
    }
    if let Some(view) = &request.view {
        if !model.views.iter().any(|candidate| {
            candidate.part == view.part && candidate.name.as_deref() == Some(view.name.as_str())
        }) {
            return Err(format!(
                "Timeline view {:?} was not resolved in part {}",
                view.name, view.part
            ));
        }
    }
    if let Some(cache) = &request.cache {
        if !model
            .caches
            .iter()
            .any(|candidate| candidate.part == cache.part)
        {
            return Err(format!(
                "Timeline cache part {} was not resolved",
                cache.part
            ));
        }
        if let Some(delta) = &cache.patch.connections {
            for target in &delta.add {
                if target.tab_id != u32::MAX
                    && !model
                        .pivot_tables
                        .iter()
                        .any(|pivot| pivot.sheet_id == target.tab_id && pivot.name == target.name)
                {
                    return Err(format!(
                        "Timeline connection target {} / {:?} is not an existing PivotTable",
                        target.tab_id, target.name
                    ));
                }
            }
        }
    }
    if let (Some(view), Some(cache)) = (&request.view, &request.cache) {
        let resolved = model
            .views
            .iter()
            .find(|candidate| {
                candidate.part == view.part && candidate.name.as_deref() == Some(view.name.as_str())
            })
            .and_then(|candidate| candidate.cache_part.as_deref());
        if resolved != Some(cache.part.as_str()) {
            return Err("Timeline view and cache edit targets are not associated".to_string());
        }
    }
    Ok(())
}

/// Atomically applies view/cache edits to an OPC part map and returns the reparsed native model.
pub(crate) fn apply_timeline_edit(
    parts: &mut BTreeMap<String, Vec<u8>>,
    request: &TimelineEditRequest,
) -> Result<TimelineWorkbookModel, String> {
    let model = parse_timeline_model(parts)?;
    validate_edit_request(&model, request)?;
    let mut replacements: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    if let Some(view) = &request.view {
        let xml = xml_part(parts, &view.part)?;
        let edited = apply_timeline_view_patch(xml, &view.name, &view.patch)?;
        replacements.insert(view.part.clone(), edited.into_bytes());
    }
    if let Some(cache) = &request.cache {
        let xml = xml_part(parts, &cache.part)?;
        let edited = apply_timeline_cache_patch(xml, &cache.patch)?;
        let current_cache = model
            .caches
            .iter()
            .find(|candidate| candidate.part == cache.part)
            .ok_or("Timeline cache disappeared after validation")?;
        let (edited_filter_type, edited_selection, edited_connections) =
            cache_filter_snapshot(&edited)?;
        let connection_changed = cache
            .patch
            .connections
            .as_ref()
            .is_some_and(|delta| !delta.add.is_empty() || !delta.remove.is_empty());
        let state_changed = !matches!(cache.patch.selection, PatchField::Missing)
            || !matches!(cache.patch.filter_type, PatchField::Missing);

        // A Timeline range is not functional in desktop Excel unless every connected native
        // PivotTable carries the paired date filter.  Build that package-level delta before
        // committing any touched part, so a missing cache field or malformed filter leaves the
        // caller's OPC map unchanged.
        if state_changed || connection_changed {
            let active_selection = (edited_filter_type.as_deref() == Some("dateBetween"))
                .then_some(edited_selection.as_ref())
                .flatten();
            let old_keys: HashSet<(u32, String)> = current_cache
                .connections
                .iter()
                .map(|connection| (connection.tab_id, connection.name.clone()))
                .collect();
            let new_keys: HashSet<(u32, String)> = edited_connections
                .iter()
                .map(|connection| (connection.tab_id, connection.name.clone()))
                .collect();
            let mut targets: BTreeMap<(u32, String), Option<&TimelineRange>> = BTreeMap::new();
            if state_changed {
                for key in &new_keys {
                    targets.insert(key.clone(), active_selection);
                }
            } else {
                for key in new_keys.difference(&old_keys) {
                    targets.insert(key.clone(), active_selection);
                }
            }
            for key in old_keys.difference(&new_keys) {
                targets.insert(key.clone(), None);
            }
            let source_name = current_cache
                .source_name
                .as_deref()
                .ok_or("Timeline cache has no sourceName")?;
            for ((tab_id, pivot_name), selection) in targets {
                if tab_id == u32::MAX {
                    // Non-worksheet OLAP PivotTables are filtered by timelinePivotFilter rather
                    // than the worksheet PivotTable `filters` collection.
                    continue;
                }
                let Some(pivot) = model
                    .pivot_tables
                    .iter()
                    .find(|pivot| pivot.sheet_id == tab_id && pivot.name == pivot_name)
                else {
                    // A broken pre-existing relationship is reported in the model but must not
                    // make an unrelated Timeline view edit destructive.
                    continue;
                };
                let pivot_cache_part = pivot.cache_part.as_deref().ok_or_else(|| {
                    format!("PivotTable {} has no PivotCache relationship", pivot.part)
                })?;
                let field_index = pivot_cache_field_index(parts, pivot_cache_part, source_name)?;
                let pivot_xml = replacements
                    .get(&pivot.part)
                    .map(Vec::as_slice)
                    .map(std::str::from_utf8)
                    .transpose()
                    .map_err(|error| format!("{} UTF-8: {error}", pivot.part))?
                    .unwrap_or(xml_part(parts, &pivot.part)?);
                let pivot_edited = apply_pivot_table_timeline_filter(
                    pivot_xml,
                    field_index,
                    source_name,
                    selection,
                    model.date_1904,
                )?;
                replacements.insert(pivot.part.clone(), pivot_edited.into_bytes());
            }
        }
        replacements.insert(cache.part.clone(), edited.into_bytes());
    }
    for (part, bytes) in replacements {
        parts.insert(part, bytes);
    }
    parse_timeline_model(parts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Read;

    fn fixture_parts() -> BTreeMap<String, Vec<u8>> {
        [
            (
                "_rels/.rels",
                br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="office-weird" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/book-custom.xml"/></Relationships>"#.as_slice(),
            ),
            (
                "xl/book-custom.xml",
                br#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:x15="http://schemas.microsoft.com/office/spreadsheetml/2010/11/main"><sheets><sheet name="Dashboard" sheetId="7" r:id="sheet-z"/><sheet name="Other" sheetId="12" r:id="sheet-y"/></sheets><pivotCaches><pivotCache cacheId="55" r:id="pc-z"/></pivotCaches><extLst><ext uri="{A2CB5862-8E78-49c6-8D9D-AF26E26ADB89}"><x15:timelineCachePivotCaches><pivotCache cacheId="55"/></x15:timelineCachePivotCaches></ext><ext uri="{D0CA8CA8-9F24-4464-BF8E-62219DCF47F9}"><x15:timelineCacheRefs><x15:timelineCacheRef r:id="tl-cache-random"/></x15:timelineCacheRefs></ext></extLst></workbook>"#.as_slice(),
            ),
            (
                "xl/_rels/book-custom.xml.rels",
                br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="sheet-z" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/dashboard-custom.xml"/><Relationship Id="sheet-y" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/other.xml"/><Relationship Id="pc-z" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotCacheDefinition" Target="pivotCache/cache-arbitrary.xml"/><Relationship Id="tl-cache-random" Type="http://schemas.microsoft.com/office/2010/relationships/TimelineCache" Target="timelineCaches/cache-not-numbered.xml"/></Relationships>"#.as_slice(),
            ),
            (
                "xl/worksheets/dashboard-custom.xml",
                br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:x15="http://schemas.microsoft.com/office/spreadsheetml/2010/11/main"><sheetData/><pivotTableParts count="1"><pivotTablePart r:id="pivot-any"/></pivotTableParts><extLst><ext uri="opaque"><x15:timelineRefs><x15:timelineRef r:id="timeline-any"/></x15:timelineRefs></ext></extLst></worksheet>"#.as_slice(),
            ),
            (
                "xl/worksheets/_rels/dashboard-custom.xml.rels",
                br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="pivot-any" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotTable" Target="../pivotTables/sales-custom.xml"/><Relationship Id="timeline-any" Type="http://schemas.microsoft.com/office/2010/relationships/Timeline" Target="../timelines/views-custom.xml"/></Relationships>"#.as_slice(),
            ),
            (
                "xl/worksheets/other.xml",
                br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheetData/><pivotTableParts count="1"><pivotTablePart r:id="pivot-other"/></pivotTableParts></worksheet>"#.as_slice(),
            ),
            (
                "xl/worksheets/_rels/other.xml.rels",
                br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="pivot-other" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotTable" Target="../pivotTables/other-custom.xml"/></Relationships>"#.as_slice(),
            ),
            (
                "xl/pivotTables/sales-custom.xml",
                br#"<pivotTableDefinition xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" name="SalesPivot" cacheId="55" createdVersion="6"><location ref="A3:D20"/></pivotTableDefinition>"#.as_slice(),
            ),
            (
                "xl/pivotTables/_rels/sales-custom.xml.rels",
                br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="cache-link" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotCacheDefinition" Target="../pivotCache/cache-arbitrary.xml"/></Relationships>"#.as_slice(),
            ),
            (
                "xl/pivotTables/other-custom.xml",
                br#"<pivotTableDefinition xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" name="OtherPivot" cacheId="55" createdVersion="6"><location ref="F3:I20"/></pivotTableDefinition>"#.as_slice(),
            ),
            (
                "xl/pivotTables/_rels/other-custom.xml.rels",
                br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="cache-link-other" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotCacheDefinition" Target="../pivotCache/cache-arbitrary.xml"/></Relationships>"#.as_slice(),
            ),
            (
                "xl/pivotCache/cache-arbitrary.xml",
                br#"<pivotCacheDefinition xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" vendor="keep"><cacheSource type="worksheet"/><cacheFields count="2"><cacheField name="OrderDate"/><cacheField name="Amount"/></cacheFields><extLst><ext uri="opaque"><future/></ext><ext uri="x14"><x14:pivotCacheDefinition xmlns:x14="http://schemas.microsoft.com/office/spreadsheetml/2009/9/main" pivotCacheId="77" vendorX14="keep"/></ext></extLst></pivotCacheDefinition>"#.as_slice(),
            ),
            (
                "xl/timelineCaches/cache-not-numbered.xml",
                br#"<?xml version="1.0" encoding="UTF-8"?><x15:timelineCacheDefinition xmlns:x15="http://schemas.microsoft.com/office/spreadsheetml/2010/11/main" xmlns:xr10="http://schemas.microsoft.com/office/spreadsheetml/2016/revision10" name="TimelineCache_OrderDate" sourceName="OrderDate" xr10:uid="{CACHE-UID}" vendor="keep"><x15:pivotTables><x15:pivotTable tabId="7" name="SalesPivot" opaque="stay"/></x15:pivotTables><x15:state singleRangeFilterState="true" minimalRefreshVersion="0" lastRefreshVersion="6" pivotCacheId="77" filterType="dateBetween" filterId="8" filterTabId="7" filterPivotName="SalesPivot" vendorState="keep"><x15:selection startDate="2024-01-01T00:00:00" endDate="2024-03-31T23:59:59" vendorRange="keep"/><x15:bounds startDate="2023-01-01T00:00:00" endDate="2025-12-31T23:59:59"/><x15:extLst><x15:ext uri="opaque"><future keep="byte-exact"/></x15:ext></x15:extLst></x15:state><x15:extLst><x15:ext uri="root-opaque"><vendorPayload/></x15:ext></x15:extLst></x15:timelineCacheDefinition>"#.as_slice(),
            ),
            (
                "xl/timelines/views-custom.xml",
                br#"<?xml version="1.0" encoding="UTF-8"?><x15:timelines xmlns:x15="http://schemas.microsoft.com/office/spreadsheetml/2010/11/main" xmlns:xr10="http://schemas.microsoft.com/office/spreadsheetml/2016/revision10"><x15:timeline name="Timeline_OrderDate" xr10:uid="{VIEW-UID}" cache="TimelineCache_OrderDate" caption="Order Date" showHeader="true" showSelectionLabel="1" showTimeLevel="true" showHorizontalScrollbar="1" level="2" selectionLevel="3" scrollPosition="2024-01-01T00:00:00" style="TimelineStyleLight2" vendor="keep"><x15:extLst><x15:ext uri="opaque"><future keep="exact"/></x15:ext></x15:extLst></x15:timeline></x15:timelines>"#.as_slice(),
            ),
        ]
        .into_iter()
        .map(|(path, bytes)| (path.to_string(), bytes.to_vec()))
        .collect()
    }

    #[test]
    fn resolves_arbitrary_parts_relationships_and_filter_graph() {
        let model = parse_timeline_model(&fixture_parts()).unwrap();
        assert_eq!(model.workbook_part, "xl/book-custom.xml");
        assert_eq!(model.timeline_cache_pivot_cache_ids, vec![55]);
        assert_eq!(model.pivot_caches.len(), 1);
        assert_eq!(model.pivot_caches[0].pivot_cache_id, Some(77));
        assert_eq!(model.pivot_tables.len(), 2);
        assert_eq!(model.caches.len(), 1);
        let cache = &model.caches[0];
        assert_eq!(cache.part, "xl/timelineCaches/cache-not-numbered.xml");
        assert_eq!(cache.name.as_deref(), Some("TimelineCache_OrderDate"));
        assert_eq!(cache.state.pivot_cache_id, Some(77));
        assert_eq!(
            cache.state.pivot_cache_part.as_deref(),
            Some("xl/pivotCache/cache-arbitrary.xml")
        );
        assert_eq!(cache.state.filter_type.as_deref(), Some("dateBetween"));
        assert_eq!(
            cache.state.selection.as_ref().unwrap().start_date,
            "2024-01-01T00:00:00"
        );
        assert_eq!(cache.connections.len(), 1);
        assert!(cache.connections[0].exists);
        assert_eq!(cache.connections[0].sheet.as_deref(), Some("Dashboard"));
        assert_eq!(model.views.len(), 1);
        let view = &model.views[0];
        assert_eq!(view.sheet_id, 7);
        assert_eq!(view.caption.as_deref(), Some("Order Date"));
        assert_eq!(view.cache_part.as_deref(), Some(cache.part.as_str()));
        assert_eq!(cache.view_parts, vec![view.part.clone()]);
    }

    #[test]
    fn view_empty_and_semantically_equal_patch_is_byte_exact() {
        let parts = fixture_parts();
        let original = xml_part(&parts, "xl/timelines/views-custom.xml").unwrap();
        assert_eq!(
            apply_timeline_view_patch(
                original,
                "Timeline_OrderDate",
                &TimelineViewPatch::default()
            )
            .unwrap(),
            original
        );
        let equal: TimelineViewPatch = serde_json::from_value(serde_json::json!({
            "caption": "Order Date",
            "level": 2,
            "selectionLevel": 3,
            "scrollPosition": "2024-01-01T00:00:00",
            "style": "TimelineStyleLight2",
            "showHeader": true,
            "showSelectionLabel": true,
            "showTimeLevel": true,
            "showHorizontalScrollbar": true
        }))
        .unwrap();
        assert_eq!(
            apply_timeline_view_patch(original, "Timeline_OrderDate", &equal).unwrap(),
            original
        );
    }

    #[test]
    fn view_patch_changes_only_requested_attributes_and_keeps_extensions() {
        let parts = fixture_parts();
        let original = xml_part(&parts, "xl/timelines/views-custom.xml").unwrap();
        let patch: TimelineViewPatch = serde_json::from_value(serde_json::json!({
            "caption": "Fiscal & Calendar",
            "level": 1,
            "selectionLevel": 1,
            "style": null,
            "showHeader": false,
            "scrollPosition": "2024-04-01T00:00:00Z"
        }))
        .unwrap();
        let edited = apply_timeline_view_patch(original, "Timeline_OrderDate", &patch).unwrap();
        assert!(edited.contains("caption=\"Fiscal &amp; Calendar\""));
        assert!(edited.contains("level=\"1\""));
        assert!(edited.contains("selectionLevel=\"1\""));
        assert!(edited.contains("showHeader=\"0\""));
        assert!(edited.contains("scrollPosition=\"2024-04-01T00:00:00Z\""));
        assert!(!edited.contains(" style="));
        assert!(edited.contains("vendor=\"keep\""));
        assert!(edited.contains("<future keep=\"exact\"/>"));
        assert!(
            apply_timeline_view_patch(
                original,
                "Timeline_OrderDate",
                &serde_json::from_value(serde_json::json!({"level": 4})).unwrap()
            )
            .is_err()
        );
    }

    #[test]
    fn cache_range_and_state_patch_is_lossless_and_no_op_aware() {
        let parts = fixture_parts();
        let original = xml_part(&parts, "xl/timelineCaches/cache-not-numbered.xml").unwrap();
        assert_eq!(
            apply_timeline_cache_patch(original, &TimelineCachePatch::default()).unwrap(),
            original
        );
        let equal: TimelineCachePatch = serde_json::from_value(serde_json::json!({
            "selection": {
                "startDate": "2024-01-01T00:00:00",
                "endDate": "2024-03-31T23:59:59"
            },
            "bounds": {
                "startDate": "2023-01-01T00:00:00",
                "endDate": "2025-12-31T23:59:59"
            },
            "filterType": "dateBetween",
            "singleRangeFilterState": true
        }))
        .unwrap();
        assert_eq!(
            apply_timeline_cache_patch(original, &equal).unwrap(),
            original
        );

        let patch: TimelineCachePatch = serde_json::from_value(serde_json::json!({
            "selection": {
                "startDate": "2024-04-01T00:00:00",
                "endDate": "2024-06-30T23:59:59"
            },
            "bounds": {
                "startDate": "2022-01-01T00:00:00",
                "endDate": "2026-12-31T23:59:59"
            },
            "singleRangeFilterState": false
        }))
        .unwrap();
        let edited = apply_timeline_cache_patch(original, &patch).unwrap();
        assert!(edited.contains("singleRangeFilterState=\"0\""));
        assert!(edited.contains("startDate=\"2024-04-01T00:00:00\""));
        assert!(edited.contains("endDate=\"2024-06-30T23:59:59\""));
        assert!(edited.contains("startDate=\"2022-01-01T00:00:00\""));
        assert!(edited.contains("endDate=\"2026-12-31T23:59:59\""));
        assert!(edited.contains("vendorRange=\"keep\""));
        assert!(edited.contains("vendorState=\"keep\""));
        assert!(edited.contains("<future keep=\"byte-exact\"/>"));
        assert!(edited.contains("<vendorPayload/>"));
    }

    #[test]
    fn connection_delta_preserves_existing_opaque_xml_and_validates_clear() {
        let parts = fixture_parts();
        let original = xml_part(&parts, "xl/timelineCaches/cache-not-numbered.xml").unwrap();
        let add: TimelineCachePatch = serde_json::from_value(serde_json::json!({
            "connections": {
                "add": [{"tabId": 12, "name": "OtherPivot"}]
            }
        }))
        .unwrap();
        let added = apply_timeline_cache_patch(original, &add).unwrap();
        assert!(
            added.contains("<x15:pivotTable tabId=\"7\" name=\"SalesPivot\" opaque=\"stay\"/>")
        );
        assert!(added.contains("<x15:pivotTable tabId=\"12\" name=\"OtherPivot\"/>"));
        assert_eq!(apply_timeline_cache_patch(&added, &add).unwrap(), added);

        let remove: TimelineCachePatch = serde_json::from_value(serde_json::json!({
            "connections": {
                "remove": [{"tabId": 7, "name": "SalesPivot"}]
            }
        }))
        .unwrap();
        let removed = apply_timeline_cache_patch(&added, &remove).unwrap();
        assert!(!removed.contains("name=\"SalesPivot\" opaque=\"stay\""));
        assert!(removed.contains("name=\"OtherPivot\""));

        let clear: TimelineCachePatch = serde_json::from_value(serde_json::json!({
            "connections": {
                "remove": [{"tabId": 12, "name": "OtherPivot"}]
            }
        }))
        .unwrap();
        let cleared = apply_timeline_cache_patch(&removed, &clear).unwrap();
        assert!(!cleared.contains("<x15:pivotTables>"));
        assert!(cleared.contains("vendor=\"keep\""));
    }

    #[test]
    fn moving_period_and_pivot_filter_require_explicit_destructive_acknowledgement() {
        let xml = r#"<x15:timelineCacheDefinition xmlns:x15="urn:x" name="C" sourceName="D"><x15:state minimalRefreshVersion="0" lastRefreshVersion="1" pivotCacheId="2" filterType="nextMonth"><x15:selection startDate="2024-01-01T00:00:00" endDate="2024-01-31T00:00:00"/><x15:bounds startDate="2020-01-01T00:00:00" endDate="2030-01-01T00:00:00"/><x15:movingPeriodState vendor="opaque"/></x15:state><x15:timelinePivotFilter vendor="opaque"/><x15:extLst><future/></x15:extLst></x15:timelineCacheDefinition>"#;
        let unsafe_patch: TimelineCachePatch = serde_json::from_value(serde_json::json!({
            "selection": null,
            "filterType": "unknown"
        }))
        .unwrap();
        assert!(apply_timeline_cache_patch(xml, &unsafe_patch).is_err());
        let acknowledged: TimelineCachePatch = serde_json::from_value(serde_json::json!({
            "selection": null,
            "filterType": "unknown",
            "clearMovingPeriodState": true,
            "clearTimelinePivotFilter": true
        }))
        .unwrap();
        let edited = apply_timeline_cache_patch(xml, &acknowledged).unwrap();
        assert!(!edited.contains("selection"));
        assert!(!edited.contains("movingPeriodState"));
        assert!(!edited.contains("timelinePivotFilter"));
        assert!(edited.contains("<x15:extLst><future/></x15:extLst>"));
    }

    #[test]
    fn atomic_edit_validates_relationship_targets_before_writing() {
        let mut parts = fixture_parts();
        let original = parts.clone();
        let invalid: TimelineEditRequest = serde_json::from_value(serde_json::json!({
            "cache": {
                "part": "xl/timelineCaches/cache-not-numbered.xml",
                "patch": {"connections": {"add": [{"tabId": 99, "name": "Ghost"}]}}
            }
        }))
        .unwrap();
        assert!(apply_timeline_edit(&mut parts, &invalid).is_err());
        assert_eq!(parts, original);

        let request: TimelineEditRequest = serde_json::from_value(serde_json::json!({
            "view": {
                "part": "xl/timelines/views-custom.xml",
                "name": "Timeline_OrderDate",
                "patch": {"caption": "Quarter selector", "level": 1}
            },
            "cache": {
                "part": "xl/timelineCaches/cache-not-numbered.xml",
                "patch": {"connections": {"add": [{"tabId": 12, "name": "OtherPivot"}]}}
            }
        }))
        .unwrap();
        let model = apply_timeline_edit(&mut parts, &request).unwrap();
        assert_eq!(model.views[0].caption.as_deref(), Some("Quarter selector"));
        assert_eq!(model.views[0].level, Some(1));
        assert_eq!(model.caches[0].connections.len(), 2);
        assert!(
            model.caches[0]
                .connections
                .iter()
                .all(|connection| connection.exists)
        );
        let other_pivot = xml_part(&parts, "xl/pivotTables/other-custom.xml").unwrap();
        assert!(other_pivot.contains("type=\"dateBetween\""));
        assert!(other_pivot.contains("<x15:pivotFilter useWholeDay=\"1\"/>"));
    }

    #[test]
    fn selection_edit_synchronizes_native_pivot_filter_and_is_byte_exact_when_replayed() {
        let mut parts = fixture_parts();
        let request: TimelineEditRequest = serde_json::from_value(serde_json::json!({
            "cache": {
                "part": "xl/timelineCaches/cache-not-numbered.xml",
                "patch": {
                    "selection": {
                        "startDate": "2025-03-01T00:00:00",
                        "endDate": "2025-08-31T00:00:00"
                    }
                }
            }
        }))
        .unwrap();
        let model = apply_timeline_edit(&mut parts, &request).unwrap();
        assert_eq!(
            model.caches[0].state.filter_type.as_deref(),
            Some("dateBetween")
        );
        let pivot = xml_part(&parts, "xl/pivotTables/sales-custom.xml").unwrap();
        assert!(pivot.contains("operator=\"greaterThanOrEqual\" val=\"45717\""));
        assert!(pivot.contains("operator=\"lessThanOrEqual\" val=\"45900\""));
        assert!(pivot.contains("<x15:pivotFilter useWholeDay=\"1\"/>"));
        let after_first = parts.clone();
        apply_timeline_edit(&mut parts, &request).unwrap();
        assert_eq!(parts, after_first);

        let clear: TimelineEditRequest = serde_json::from_value(serde_json::json!({
            "cache": {
                "part": "xl/timelineCaches/cache-not-numbered.xml",
                "patch": {"selection": null}
            }
        }))
        .unwrap();
        let cleared = apply_timeline_edit(&mut parts, &clear).unwrap();
        assert_eq!(
            cleared.caches[0].state.filter_type.as_deref(),
            Some("unknown")
        );
        assert!(cleared.caches[0].state.selection.is_none());
        assert!(
            !xml_part(&parts, "xl/pivotTables/sales-custom.xml")
                .unwrap()
                .contains("<filters")
        );
    }

    fn read_xlsx_fixture(path: &str) -> Option<BTreeMap<String, Vec<u8>>> {
        let file = File::open(path).ok()?;
        let mut archive = zip::ZipArchive::new(file).ok()?;
        let mut parts = BTreeMap::new();
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).ok()?;
            if entry.is_dir() {
                continue;
            }
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).ok()?;
            parts.insert(entry.name().replace('\\', "/"), bytes);
        }
        Some(parts)
    }

    #[test]
    fn reads_real_excel_16_timeline_and_filtered_fixture() {
        let Some(unfiltered) =
            read_xlsx_fixture("test-fixtures/unicell-pivot-slicer-timeline-unfiltered.xlsx")
        else {
            // The fixture is generated by the Excel 16 compatibility harness and is optional in
            // clean CI checkouts; the embedded tests above exercise the same native structures.
            return;
        };
        let model = parse_timeline_model(&unfiltered).unwrap();
        assert_eq!(model.caches.len(), 1);
        assert_eq!(model.views.len(), 1);
        assert_eq!(
            model.caches[0].state.filter_type.as_deref(),
            Some("unknown")
        );
        assert!(model.caches[0].state.selection.is_none());
        assert_eq!(model.caches[0].state.pivot_cache_id, Some(1_986_639_402));
        assert_eq!(
            model.caches[0].state.pivot_cache_part.as_deref(),
            Some("xl/pivotCache/pivotCacheDefinition1.xml")
        );
        assert_eq!(model.views[0].caption.as_deref(), Some("Date Timeline"));
        assert_eq!(model.views[0].level, Some(2));
        assert_eq!(model.views[0].selection_level, Some(2));

        let mut package_edited = unfiltered.clone();
        let request = TimelineEditRequest {
            view: None,
            cache: Some(TimelineCacheEditTarget {
                part: model.caches[0].part.clone(),
                patch: serde_json::from_value(serde_json::json!({
                    "selection": {
                        "startDate": "2025-03-01T00:00:00",
                        "endDate": "2025-08-31T00:00:00"
                    }
                }))
                .unwrap(),
            }),
        };
        let edited_model = apply_timeline_edit(&mut package_edited, &request).unwrap();
        assert_eq!(
            edited_model.caches[0].state.filter_type.as_deref(),
            Some("dateBetween")
        );
        let generated_pivot = xml_part(&package_edited, "xl/pivotTables/pivotTable1.xml").unwrap();
        assert!(
            generated_pivot.contains("operator=\"greaterThanOrEqual\" val=\"45717\""),
            "generated PivotTable filter:\n{generated_pivot}"
        );
        assert!(
            generated_pivot.contains("operator=\"lessThanOrEqual\" val=\"45900\""),
            "generated PivotTable filter:\n{generated_pivot}"
        );

        let Some(filtered) =
            read_xlsx_fixture("test-fixtures/unicell-pivot-slicer-timeline-filtered.xlsx")
        else {
            return;
        };
        let filtered_model = parse_timeline_model(&filtered).unwrap();
        let cache = &filtered_model.caches[0];
        assert_eq!(cache.state.filter_type.as_deref(), Some("dateBetween"));
        assert_eq!(
            cache.state.selection,
            Some(TimelineRange {
                start_date: "2025-03-01T00:00:00".to_string(),
                end_date: "2025-08-31T00:00:00".to_string(),
            })
        );
        let view = &filtered_model.views[0];
        assert_eq!(view.show_header, Some(false));
        assert_eq!(view.show_selection_label, Some(false));
        assert_eq!(view.show_time_level, Some(false));
        assert_eq!(view.show_horizontal_scrollbar, Some(false));
        let pivot = xml_part(&filtered, "xl/pivotTables/pivotTable1.xml").unwrap();
        let equal = apply_pivot_table_timeline_filter(
            pivot,
            0,
            "Date",
            cache.state.selection.as_ref(),
            false,
        )
        .unwrap();
        assert_eq!(equal, pivot);
    }

    #[test]
    fn rejects_invalid_dates_unknown_json_and_ambiguous_connection_delta() {
        assert!(validate_date_time("2023-02-29T00:00:00").is_err());
        assert!(validate_date_time("2024-02-29T23:59:59+08:00").is_ok());
        assert!(
            serde_json::from_value::<TimelineViewPatch>(serde_json::json!({
                "unsupported": true
            }))
            .is_err()
        );
        assert!(
            validate_connection_delta(&TimelineConnectionDelta {
                add: vec![TimelineConnectionTarget {
                    tab_id: 7,
                    name: "SalesPivot".to_string(),
                }],
                remove: vec![TimelineConnectionTarget {
                    tab_id: 7,
                    name: "SalesPivot".to_string(),
                }],
            })
            .is_err()
        );
        let parts = fixture_parts();
        let cache = xml_part(&parts, "xl/timelineCaches/cache-not-numbered.xml").unwrap();
        let unsupported: TimelineCachePatch =
            serde_json::from_value(serde_json::json!({"filterType": "nextMonth"})).unwrap();
        assert!(apply_timeline_cache_patch(cache, &unsupported).is_err());
        let incomplete_clear: TimelineCachePatch =
            serde_json::from_value(serde_json::json!({"filterType": "unknown"})).unwrap();
        assert!(apply_timeline_cache_patch(cache, &incomplete_clear).is_err());
    }
}
