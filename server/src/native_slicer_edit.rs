//! Lossless native OOXML slicer reader and differential editor.
//!
//! Slicers are not a single XML object. A workbook owns slicer-cache parts, worksheets own
//! slicer-view parts, slicer views refer to caches by name, and slicer caches refer to
//! PivotTables by `(sheetId, PivotTable name)`. Office 2010 objects use the `x14` namespace,
//! while newer table/non-worksheet extensions are commonly stored below `x15` extension
//! nodes. This module resolves that OPC graph through relationships and content types; it does
//! not rely on `slicer1.xml`, `slicerCache1.xml`, or `rId1` naming conventions.
//!
//! Editing is deliberately differential. Existing start tags are patched in place and child
//! elements are moved as their original byte ranges. Unknown attributes, namespace declarations,
//! children, `extLst` payloads, and vendor markup are therefore retained. An empty/no-op patch is
//! byte-exact. Creating new OPC parts is intentionally outside this module: adding an entry to an
//! existing slicers/cache part is supported, but callers must already have (or clone) its DrawingML
//! anchor when a new visible slicer is desired.

use roxmltree::{Document, Node};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ops::Range;

const REL_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const X14_NS: &str = "http://schemas.microsoft.com/office/spreadsheetml/2009/9/main";
const X15_NS: &str = "http://schemas.microsoft.com/office/spreadsheetml/2010/11/main";
const SLICER_CONTENT_TYPE: &str = "application/vnd.ms-excel.slicer+xml";
const SLICER_CACHE_CONTENT_TYPE: &str = "application/vnd.ms-excel.slicerCache+xml";
const DRAWING_CONTENT_TYPE: &str = "application/vnd.openxmlformats-officedocument.drawing+xml";

#[derive(Clone, Debug)]
struct Relationship {
    id: String,
    kind: String,
    resolved_part: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct ContentTypes {
    overrides: HashMap<String, String>,
    defaults: HashMap<String, String>,
}

#[derive(Clone, Debug)]
struct SheetInfo {
    name: String,
    sheet_id: u64,
    part: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeSlicerModel {
    pub workbook_part: String,
    pub caches: Vec<NativeSlicerCache>,
    pub slicer_parts: Vec<NativeSlicerPart>,
    pub pivot_tables: Vec<NativeSlicerPivotTarget>,
    pub tables: Vec<NativeSlicerTableTarget>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeSlicerCache {
    pub part: String,
    pub relationship_id: Option<String>,
    pub content_type: Option<String>,
    pub namespace_uri: Option<String>,
    pub name: String,
    pub source_name: String,
    pub data_kind: String,
    pub pivot_cache_id: Option<u64>,
    pub table_id: Option<u64>,
    pub tabular: Option<NativeTabularSlicerOptions>,
    pub table: Option<NativeTableSlicerOptions>,
    pub connections: Vec<NativeSlicerConnection>,
    pub items: Vec<NativeSlicerItem>,
    pub olap_selections: Vec<NativeOlapSlicerSelection>,
    pub has_x15_extensions: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeTabularSlicerOptions {
    pub sort_order: String,
    pub custom_list_sort: bool,
    pub show_missing: bool,
    pub cross_filter: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeTableSlicerOptions {
    pub table_id: u64,
    pub column: u64,
    pub sort_order: String,
    pub custom_list_sort: bool,
    pub cross_filter: String,
    pub target_part: Option<String>,
    pub table_name: Option<String>,
    pub column_name: Option<String>,
    pub valid: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeSlicerConnection {
    pub source_index: usize,
    pub container: String,
    pub namespace_uri: Option<String>,
    pub tab_id: u64,
    pub name: String,
    pub target_part: Option<String>,
    pub target_cache_id: Option<u64>,
    pub target_pivot_cache_id: Option<u64>,
    pub valid: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeSlicerItem {
    pub source_index: usize,
    pub item_index: u64,
    pub label: String,
    pub value: Value,
    pub selected: bool,
    pub no_data: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeOlapSlicerSelection {
    pub source_index: usize,
    pub name: String,
    pub parents: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeSlicerPart {
    pub part: String,
    pub relationship_id: Option<String>,
    pub content_type: Option<String>,
    pub namespace_uri: Option<String>,
    pub sheet: Option<String>,
    pub sheet_id: Option<u64>,
    pub sheet_part: Option<String>,
    pub slicers: Vec<NativeSlicerView>,
    pub has_x15_extensions: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeSlicerView {
    pub source_index: usize,
    pub name: String,
    pub cache: String,
    pub cache_part: Option<String>,
    pub caption: Option<String>,
    pub start_item: u64,
    pub column_count: u64,
    pub show_caption: bool,
    pub level: u64,
    pub style: Option<String>,
    pub locked_position: bool,
    pub row_height: Option<u64>,
    pub uid: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeSlicerPivotTarget {
    pub tab_id: u64,
    pub sheet: String,
    pub sheet_part: String,
    pub name: String,
    pub part: String,
    pub cache_id: Option<u64>,
    pub cache_part: Option<String>,
    pub pivot_cache_id: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeSlicerTableTarget {
    pub table_id: u64,
    pub name: String,
    pub part: String,
    pub sheet: String,
    pub sheet_part: String,
    pub columns: Vec<NativeSlicerTableColumn>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeSlicerTableColumn {
    pub id: u64,
    pub name: String,
}

#[derive(Clone, Debug)]
struct AttributeSpan {
    name: String,
    value: String,
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
) -> impl Iterator<Item = Node<'a, 'input>> + 'a {
    node.children()
        .filter(move |child| child.is_element() && local_name(*child) == name)
}

fn relationship_id(node: Node<'_, '_>) -> Option<String> {
    node.attributes()
        .find(|attribute| attribute.name() == "id" && attribute.namespace() == Some(REL_NS))
        .map(|attribute| attribute.value().to_string())
        .or_else(|| node.attribute("id").map(str::to_string))
}

fn parse_bool(value: Option<&str>, default: bool) -> bool {
    match value {
        Some("1" | "true" | "on") => true,
        Some("0" | "false" | "off") => false,
        _ => default,
    }
}

fn parse_u64(value: Option<&str>) -> Option<u64> {
    value.and_then(|value| value.parse::<u64>().ok())
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
            .map(|mode| mode.eq_ignore_ascii_case("External"))
            .unwrap_or(false);
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

fn parse_content_types(parts: &BTreeMap<String, Vec<u8>>) -> Result<ContentTypes, String> {
    let Some(bytes) = parts.get("[Content_Types].xml") else {
        return Ok(ContentTypes::default());
    };
    let xml = std::str::from_utf8(bytes)
        .map_err(|error| format!("[Content_Types].xml UTF-8: {error}"))?;
    let document =
        Document::parse(xml).map_err(|error| format!("[Content_Types].xml XML: {error}"))?;
    let mut content_types = ContentTypes::default();
    for node in document.descendants().filter(Node::is_element) {
        match local_name(node) {
            "Override" => {
                if let (Some(part), Some(kind)) =
                    (node.attribute("PartName"), node.attribute("ContentType"))
                {
                    if let Some(part) = normalize_part_path(part) {
                        content_types.overrides.insert(part, kind.to_string());
                    }
                }
            }
            "Default" => {
                if let (Some(extension), Some(kind)) =
                    (node.attribute("Extension"), node.attribute("ContentType"))
                {
                    content_types
                        .defaults
                        .insert(extension.to_ascii_lowercase(), kind.to_string());
                }
            }
            _ => {}
        }
    }
    Ok(content_types)
}

fn content_type_for(content_types: &ContentTypes, part: &str) -> Option<String> {
    content_types.overrides.get(part).cloned().or_else(|| {
        part.rsplit_once('.')
            .and_then(|(_, extension)| content_types.defaults.get(&extension.to_ascii_lowercase()))
            .cloned()
    })
}

fn parts_with_content_type(content_types: &ContentTypes, kind: &str) -> Vec<String> {
    let mut parts: Vec<String> = content_types
        .overrides
        .iter()
        .filter_map(|(part, content_type)| (content_type == kind).then(|| part.clone()))
        .collect();
    parts.sort();
    parts
}

fn office_document_part(parts: &BTreeMap<String, Vec<u8>>) -> Result<String, String> {
    if let Some(part) = parse_relationships(parts, "")?
        .values()
        .find(|relationship| relationship.kind.ends_with("/officeDocument"))
        .and_then(|relationship| relationship.resolved_part.clone())
        .filter(|part| parts.contains_key(part))
    {
        return Ok(part);
    }
    parts
        .contains_key("xl/workbook.xml")
        .then(|| "xl/workbook.xml".to_string())
        .ok_or_else(|| "OPC package has no workbook part".to_string())
}

fn parse_xml_part<'a>(
    parts: &'a BTreeMap<String, Vec<u8>>,
    path: &str,
) -> Result<(&'a str, Document<'a>), String> {
    let bytes = parts
        .get(path)
        .ok_or_else(|| format!("missing OPC part {path}"))?;
    let xml = std::str::from_utf8(bytes).map_err(|error| format!("{path} UTF-8: {error}"))?;
    let document = Document::parse(xml).map_err(|error| format!("{path} XML: {error}"))?;
    Ok((xml, document))
}

fn workbook_sheets(
    parts: &BTreeMap<String, Vec<u8>>,
    _workbook_part: &str,
    workbook: Node<'_, '_>,
    relationships: &HashMap<String, Relationship>,
) -> Vec<SheetInfo> {
    workbook
        .descendants()
        .filter(|node| node.is_element() && local_name(*node) == "sheet")
        .filter_map(|sheet| {
            let relationship = relationships.get(&relationship_id(sheet)?)?;
            if !relationship.kind.ends_with("/worksheet") {
                return None;
            }
            let part = relationship.resolved_part.as_ref()?.clone();
            if !parts.contains_key(&part) {
                return None;
            }
            Some(SheetInfo {
                name: sheet.attribute("name").unwrap_or("").to_string(),
                sheet_id: parse_u64(sheet.attribute("sheetId"))?,
                part,
            })
        })
        .collect()
}

fn pivot_cache_extension_id(root: Node<'_, '_>) -> Option<u64> {
    root.descendants()
        .find(|node| {
            node.is_element()
                && local_name(*node) == "pivotCacheDefinition"
                && node.tag_name().namespace() == Some(X14_NS)
        })
        .and_then(|node| parse_u64(node.attribute("pivotCacheId")))
}

fn parse_pivot_targets(
    parts: &BTreeMap<String, Vec<u8>>,
    sheets: &[SheetInfo],
) -> Result<Vec<NativeSlicerPivotTarget>, String> {
    let mut targets = Vec::new();
    let mut seen = HashSet::new();
    for sheet in sheets {
        let (_, sheet_document) = parse_xml_part(parts, &sheet.part)?;
        let relationships = parse_relationships(parts, &sheet.part)?;
        for pivot in sheet_document
            .descendants()
            .filter(|node| node.is_element() && local_name(*node) == "pivotTablePart")
        {
            let Some(relationship) = relationship_id(pivot)
                .as_ref()
                .and_then(|id| relationships.get(id))
            else {
                continue;
            };
            if !relationship.kind.ends_with("/pivotTable") {
                continue;
            }
            let Some(part) = relationship.resolved_part.as_ref() else {
                continue;
            };
            let (_, pivot_document) = parse_xml_part(parts, part)?;
            let root = pivot_document.root_element();
            if local_name(root) != "pivotTableDefinition" {
                continue;
            }
            let name = root.attribute("name").unwrap_or("").to_string();
            let cache_id = parse_u64(root.attribute("cacheId"));
            let cache_part = parse_relationships(parts, part)?
                .values()
                .find(|relationship| relationship.kind.ends_with("/pivotCacheDefinition"))
                .and_then(|relationship| relationship.resolved_part.clone());
            let pivot_cache_id = cache_part
                .as_deref()
                .and_then(|cache_part| parse_xml_part(parts, cache_part).ok())
                .and_then(|(_, document)| pivot_cache_extension_id(document.root_element()));
            if seen.insert((sheet.sheet_id, name.clone())) {
                targets.push(NativeSlicerPivotTarget {
                    tab_id: sheet.sheet_id,
                    sheet: sheet.name.clone(),
                    sheet_part: sheet.part.clone(),
                    name,
                    part: part.clone(),
                    cache_id,
                    cache_part,
                    pivot_cache_id,
                });
            }
        }
    }
    targets.sort_by(|left, right| {
        left.tab_id
            .cmp(&right.tab_id)
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(targets)
}

fn parse_table_targets(
    parts: &BTreeMap<String, Vec<u8>>,
    sheets: &[SheetInfo],
) -> Result<Vec<NativeSlicerTableTarget>, String> {
    let mut targets = Vec::new();
    let mut seen = HashSet::new();
    for sheet in sheets {
        for relationship in parse_relationships(parts, &sheet.part)?.values() {
            if !relationship.kind.ends_with("/table") {
                continue;
            }
            let Some(part) = relationship.resolved_part.as_ref() else {
                continue;
            };
            let (_, document) = parse_xml_part(parts, part)?;
            let root = document.root_element();
            if local_name(root) != "table" {
                continue;
            }
            let Some(table_id) = parse_u64(root.attribute("id")) else {
                continue;
            };
            if !seen.insert(table_id) {
                continue;
            }
            targets.push(NativeSlicerTableTarget {
                table_id,
                name: root
                    .attribute("displayName")
                    .or_else(|| root.attribute("name"))
                    .unwrap_or("")
                    .to_string(),
                part: part.clone(),
                sheet: sheet.name.clone(),
                sheet_part: sheet.part.clone(),
                columns: direct_child(root, "tableColumns")
                    .into_iter()
                    .flat_map(|columns| direct_children(columns, "tableColumn"))
                    .filter_map(|column| {
                        Some(NativeSlicerTableColumn {
                            id: parse_u64(column.attribute("id"))?,
                            name: column.attribute("name").unwrap_or("").to_string(),
                        })
                    })
                    .collect(),
            });
        }
    }
    targets.sort_by_key(|target| target.table_id);
    Ok(targets)
}

fn cache_connections(
    root: Node<'_, '_>,
    pivot_targets: &[NativeSlicerPivotTarget],
) -> Vec<NativeSlicerConnection> {
    let mut result = Vec::new();
    let containers: Vec<(Node<'_, '_>, &str)> = root
        .descendants()
        .filter(|node| {
            node.is_element()
                && matches!(local_name(*node), "pivotTables" | "slicerCachePivotTables")
        })
        .map(|node| {
            let kind = if local_name(node) == "pivotTables" {
                "pivotTables"
            } else {
                "x15PivotTables"
            };
            (node, kind)
        })
        .collect();
    for (container, kind) in containers {
        for (source_index, pivot) in direct_children(container, "pivotTable").enumerate() {
            let Some(tab_id) = parse_u64(pivot.attribute("tabId")) else {
                continue;
            };
            let name = pivot.attribute("name").unwrap_or("").to_string();
            let target = pivot_targets
                .iter()
                .find(|target| target.tab_id == tab_id && target.name == name);
            result.push(NativeSlicerConnection {
                source_index,
                container: kind.to_string(),
                namespace_uri: pivot.tag_name().namespace().map(str::to_string),
                tab_id,
                name,
                target_part: target.map(|target| target.part.clone()),
                target_cache_id: target.and_then(|target| target.cache_id),
                target_pivot_cache_id: target.and_then(|target| target.pivot_cache_id),
                valid: target.is_some(),
            });
        }
    }
    result
}

fn pivot_shared_item_value(node: Node<'_, '_>) -> (String, Value) {
    let raw = node.attribute("v").unwrap_or("");
    match local_name(node) {
        "n" => raw
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(|number| (raw.to_string(), Value::Number(number)))
            .unwrap_or_else(|| (raw.to_string(), Value::String(raw.to_string()))),
        "b" => {
            let value = parse_bool(Some(raw), false);
            (value.to_string(), Value::Bool(value))
        }
        "m" => (String::new(), Value::Null),
        // Strings, dates, errors, and other future scalar types are kept as their native lexical
        // value. The label is exactly what Excel caches; callers can format dates/numbers for UI.
        _ => (raw.to_string(), Value::String(raw.to_string())),
    }
}

fn slicer_shared_items(
    parts: &BTreeMap<String, Vec<u8>>,
    pivot_targets: &[NativeSlicerPivotTarget],
    pivot_cache_id: Option<u64>,
    source_name: &str,
) -> Vec<(String, Value)> {
    let Some(cache_part) = pivot_targets
        .iter()
        .find(|target| {
            pivot_cache_id.is_some()
                && target.pivot_cache_id == pivot_cache_id
                && target.cache_part.is_some()
        })
        .and_then(|target| target.cache_part.as_deref())
    else {
        return Vec::new();
    };
    let Ok((_, document)) = parse_xml_part(parts, cache_part) else {
        return Vec::new();
    };
    let Some(field) = document.descendants().find(|node| {
        node.is_element()
            && local_name(*node) == "cacheField"
            && node.attribute("name") == Some(source_name)
    }) else {
        return Vec::new();
    };
    direct_child(field, "sharedItems")
        .into_iter()
        .flat_map(|shared| shared.children().filter(Node::is_element))
        .map(pivot_shared_item_value)
        .collect()
}

fn cache_items(root: Node<'_, '_>, shared_items: &[(String, Value)]) -> Vec<NativeSlicerItem> {
    root.descendants()
        .find(|node| node.is_element() && local_name(*node) == "tabular")
        .and_then(|tabular| direct_child(tabular, "items"))
        .into_iter()
        .flat_map(|items| direct_children(items, "i"))
        .enumerate()
        .filter_map(|(source_index, item)| {
            let item_index = parse_u64(item.attribute("x"))?;
            let (label, value) = shared_items
                .get(usize::try_from(item_index).ok()?)
                .cloned()
                .unwrap_or_else(|| (item_index.to_string(), Value::from(item_index)));
            Some(NativeSlicerItem {
                source_index,
                item_index,
                label,
                value,
                selected: parse_bool(item.attribute("s"), false),
                no_data: parse_bool(item.attribute("nd"), false),
            })
        })
        .collect()
}

fn olap_selections(root: Node<'_, '_>) -> Vec<NativeOlapSlicerSelection> {
    root.descendants()
        .find(|node| node.is_element() && local_name(*node) == "selections")
        .into_iter()
        .flat_map(|selections| direct_children(selections, "selection"))
        .enumerate()
        .map(|(source_index, selection)| NativeOlapSlicerSelection {
            source_index,
            name: selection.attribute("n").unwrap_or("").to_string(),
            parents: direct_children(selection, "p")
                .filter_map(|parent| parent.attribute("n").map(str::to_string))
                .collect(),
        })
        .collect()
}

fn tabular_options(root: Node<'_, '_>) -> Option<NativeTabularSlicerOptions> {
    let tabular = root
        .descendants()
        .find(|node| node.is_element() && local_name(*node) == "tabular")?;
    Some(NativeTabularSlicerOptions {
        sort_order: tabular
            .attribute("sortOrder")
            .unwrap_or("ascending")
            .to_string(),
        custom_list_sort: parse_bool(tabular.attribute("customListSort"), true),
        show_missing: parse_bool(tabular.attribute("showMissing"), true),
        cross_filter: tabular
            .attribute("crossFilter")
            .unwrap_or("showItemsWithDataAtTop")
            .to_string(),
    })
}

fn table_options(
    root: Node<'_, '_>,
    table_targets: &[NativeSlicerTableTarget],
) -> Option<NativeTableSlicerOptions> {
    let table = root
        .descendants()
        .find(|node| node.is_element() && local_name(*node) == "tableSlicerCache")?;
    let table_id = parse_u64(table.attribute("tableId"))?;
    let column = parse_u64(table.attribute("column"))?;
    let target = table_targets
        .iter()
        .find(|target| target.table_id == table_id);
    Some(NativeTableSlicerOptions {
        table_id,
        column,
        sort_order: table
            .attribute("sortOrder")
            .unwrap_or("ascending")
            .to_string(),
        custom_list_sort: parse_bool(table.attribute("customListSort"), true),
        cross_filter: table
            .attribute("crossFilter")
            .unwrap_or("showItemsWithDataAtTop")
            .to_string(),
        target_part: target.map(|target| target.part.clone()),
        table_name: target.map(|target| target.name.clone()),
        column_name: target.and_then(|target| {
            target
                .columns
                .iter()
                .find(|candidate| candidate.id == column)
                .map(|candidate| candidate.name.clone())
        }),
        valid: target.is_some_and(|target| {
            target
                .columns
                .iter()
                .any(|candidate| candidate.id == column)
        }),
    })
}

fn parse_cache_part(
    parts: &BTreeMap<String, Vec<u8>>,
    content_types: &ContentTypes,
    part: &str,
    relationship_id: Option<String>,
    pivot_targets: &[NativeSlicerPivotTarget],
    table_targets: &[NativeSlicerTableTarget],
) -> Result<NativeSlicerCache, String> {
    let (_, document) = parse_xml_part(parts, part)?;
    let root = document.root_element();
    if local_name(root) != "slicerCacheDefinition" {
        return Err(format!("{part} is not a slicerCacheDefinition"));
    }
    let tabular = root
        .descendants()
        .find(|node| node.is_element() && local_name(*node) == "tabular");
    let olap = root
        .descendants()
        .find(|node| node.is_element() && local_name(*node) == "olap");
    let table = root
        .descendants()
        .find(|node| node.is_element() && local_name(*node) == "tableSlicerCache");
    let (data_kind, pivot_cache_id, table_id) = if let Some(tabular) = tabular {
        (
            "tabular",
            parse_u64(tabular.attribute("pivotCacheId")),
            None,
        )
    } else if let Some(olap) = olap {
        ("olap", parse_u64(olap.attribute("pivotCacheId")), None)
    } else if let Some(table) = table {
        ("table", None, parse_u64(table.attribute("tableId")))
    } else {
        ("unknown", None, None)
    };
    let source_name = root.attribute("sourceName").unwrap_or("").to_string();
    let shared_items = slicer_shared_items(parts, pivot_targets, pivot_cache_id, &source_name);
    Ok(NativeSlicerCache {
        part: part.to_string(),
        relationship_id,
        content_type: content_type_for(content_types, part),
        namespace_uri: root.tag_name().namespace().map(str::to_string),
        name: root.attribute("name").unwrap_or("").to_string(),
        source_name,
        data_kind: data_kind.to_string(),
        pivot_cache_id,
        table_id,
        tabular: tabular_options(root),
        table: table_options(root, table_targets),
        connections: cache_connections(root, pivot_targets),
        items: cache_items(root, &shared_items),
        olap_selections: olap_selections(root),
        has_x15_extensions: root
            .descendants()
            .any(|node| node.is_element() && node.tag_name().namespace() == Some(X15_NS)),
    })
}

fn parse_slicer_view(
    node: Node<'_, '_>,
    caches: &[NativeSlicerCache],
    source_index: usize,
) -> NativeSlicerView {
    let cache = node.attribute("cache").unwrap_or("").to_string();
    NativeSlicerView {
        source_index,
        name: node.attribute("name").unwrap_or("").to_string(),
        cache_part: caches
            .iter()
            .find(|candidate| candidate.name.eq_ignore_ascii_case(&cache))
            .map(|candidate| candidate.part.clone()),
        cache,
        caption: node.attribute("caption").map(str::to_string),
        start_item: parse_u64(node.attribute("startItem")).unwrap_or(0),
        column_count: parse_u64(node.attribute("columnCount")).unwrap_or(1),
        show_caption: parse_bool(node.attribute("showCaption"), true),
        level: parse_u64(node.attribute("level")).unwrap_or(0),
        style: node.attribute("style").map(str::to_string),
        locked_position: parse_bool(node.attribute("lockedPosition"), false),
        row_height: parse_u64(node.attribute("rowHeight")),
        uid: node
            .attributes()
            .find(|attribute| attribute.name() == "uid")
            .map(|attribute| attribute.value().to_string()),
    }
}

fn parse_slicer_part(
    parts: &BTreeMap<String, Vec<u8>>,
    content_types: &ContentTypes,
    part: &str,
    relationship_id: Option<String>,
    sheet: Option<&SheetInfo>,
    caches: &[NativeSlicerCache],
) -> Result<NativeSlicerPart, String> {
    let (_, document) = parse_xml_part(parts, part)?;
    let root = document.root_element();
    if local_name(root) != "slicers" {
        return Err(format!("{part} is not a slicers part"));
    }
    Ok(NativeSlicerPart {
        part: part.to_string(),
        relationship_id,
        content_type: content_type_for(content_types, part),
        namespace_uri: root.tag_name().namespace().map(str::to_string),
        sheet: sheet.map(|sheet| sheet.name.clone()),
        sheet_id: sheet.map(|sheet| sheet.sheet_id),
        sheet_part: sheet.map(|sheet| sheet.part.clone()),
        slicers: direct_children(root, "slicer")
            .enumerate()
            .map(|(index, node)| parse_slicer_view(node, caches, index))
            .collect(),
        has_x15_extensions: root
            .descendants()
            .any(|node| node.is_element() && node.tag_name().namespace() == Some(X15_NS)),
    })
}

fn relationship_owned_parts(
    relationships: &HashMap<String, Relationship>,
    suffix: &str,
) -> Vec<(String, String)> {
    let mut result: Vec<(String, String)> = relationships
        .values()
        .filter(|relationship| relationship.kind.ends_with(suffix))
        .filter_map(|relationship| {
            Some((relationship.id.clone(), relationship.resolved_part.clone()?))
        })
        .collect();
    result.sort_by(|left, right| left.1.cmp(&right.1));
    result
}

/// Resolves all native slicer caches, slicer-view parts, and PivotTable connection targets.
pub(crate) fn inspect_native_slicers(
    parts: &BTreeMap<String, Vec<u8>>,
) -> Result<NativeSlicerModel, String> {
    let content_types = parse_content_types(parts)?;
    let workbook_part = office_document_part(parts)?;
    let (_, workbook_document) = parse_xml_part(parts, &workbook_part)?;
    let workbook = workbook_document.root_element();
    let workbook_relationships = parse_relationships(parts, &workbook_part)?;
    let sheets = workbook_sheets(parts, &workbook_part, workbook, &workbook_relationships);
    let pivot_tables = parse_pivot_targets(parts, &sheets)?;
    let tables = parse_table_targets(parts, &sheets)?;

    let mut cache_owners: HashMap<String, String> =
        relationship_owned_parts(&workbook_relationships, "/slicerCache")
            .into_iter()
            .map(|(id, part)| (part, id))
            .collect();
    for part in parts_with_content_type(&content_types, SLICER_CACHE_CONTENT_TYPE) {
        cache_owners.entry(part).or_default();
    }
    let mut cache_parts: Vec<String> = cache_owners.keys().cloned().collect();
    cache_parts.sort();
    let mut caches = Vec::new();
    for part in cache_parts {
        caches.push(parse_cache_part(
            parts,
            &content_types,
            &part,
            cache_owners.get(&part).filter(|id| !id.is_empty()).cloned(),
            &pivot_tables,
            &tables,
        )?);
    }

    let mut slicer_owners: HashMap<String, (String, SheetInfo)> = HashMap::new();
    for sheet in &sheets {
        let relationships = parse_relationships(parts, &sheet.part)?;
        for (id, part) in relationship_owned_parts(&relationships, "/slicer") {
            slicer_owners.insert(part, (id, sheet.clone()));
        }
    }
    let mut all_slicer_parts: BTreeSet<String> = slicer_owners.keys().cloned().collect();
    all_slicer_parts.extend(parts_with_content_type(&content_types, SLICER_CONTENT_TYPE));
    let mut slicer_parts = Vec::new();
    for part in all_slicer_parts {
        let owner = slicer_owners.get(&part);
        slicer_parts.push(parse_slicer_part(
            parts,
            &content_types,
            &part,
            owner.map(|(id, _)| id.clone()),
            owner.map(|(_, sheet)| sheet),
            &caches,
        )?);
    }

    let mut warnings = Vec::new();
    let mut cache_names = HashSet::new();
    for cache in &caches {
        if !cache_names.insert(cache.name.to_ascii_lowercase()) {
            warnings.push(format!("duplicate slicer cache name {}", cache.name));
        }
        if cache.content_type.as_deref() != Some(SLICER_CACHE_CONTENT_TYPE) {
            warnings.push(format!(
                "{} has missing/unexpected slicer-cache content type",
                cache.part
            ));
        }
        for connection in &cache.connections {
            if !connection.valid {
                warnings.push(format!(
                    "{} references missing PivotTable {}:{}",
                    cache.part, connection.tab_id, connection.name
                ));
            } else if cache.pivot_cache_id.is_some()
                && connection.target_pivot_cache_id.is_some()
                && cache.pivot_cache_id != connection.target_pivot_cache_id
            {
                warnings.push(format!(
                    "{} connection {}:{} uses a different pivotCacheId",
                    cache.part, connection.tab_id, connection.name
                ));
            }
        }
        if cache.data_kind == "table" && cache.table.as_ref().is_none_or(|table| !table.valid) {
            warnings.push(format!(
                "{} references a missing table/table-column target",
                cache.part
            ));
        }
    }
    let mut slicer_names = HashSet::new();
    for part in &slicer_parts {
        if part.content_type.as_deref() != Some(SLICER_CONTENT_TYPE) {
            warnings.push(format!(
                "{} has missing/unexpected slicer content type",
                part.part
            ));
        }
        for slicer in &part.slicers {
            if !slicer_names.insert(slicer.name.to_ascii_lowercase()) {
                warnings.push(format!("duplicate slicer view name {}", slicer.name));
            }
            if slicer.cache_part.is_none() {
                warnings.push(format!(
                    "{} slicer {} references missing cache {}",
                    part.part, slicer.name, slicer.cache
                ));
            }
        }
    }
    Ok(NativeSlicerModel {
        workbook_part,
        caches,
        slicer_parts,
        pivot_tables,
        tables,
        warnings,
    })
}

/// JSON integration helper with the stable shape produced by [`NativeSlicerModel`].
pub(crate) fn parse_slicer_model(parts: &BTreeMap<String, Vec<u8>>) -> Result<Value, String> {
    serde_json::to_value(inspect_native_slicers(parts)?)
        .map_err(|error| format!("serialize slicer model: {error}"))
}

fn xml_escape_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn scan_open_tag_end(xml: &str, start: usize) -> Result<usize, String> {
    let bytes = xml.as_bytes();
    if bytes.get(start) != Some(&b'<') {
        return Err("element range does not begin with '<'".to_string());
    }
    let mut cursor = start;
    let mut quote = None;
    while cursor < bytes.len() {
        let current = bytes[cursor] as char;
        if let Some(open) = quote {
            if current == open {
                quote = None;
            }
        } else if current == '\'' || current == '"' {
            quote = Some(current);
        } else if current == '>' {
            return Ok(cursor + 1);
        }
        cursor += 1;
    }
    Err("XML element start tag is not closed".to_string())
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
            value: tag[value_start..value_end].to_string(),
            value_range: value_start..value_end,
            full_range: whitespace_start..cursor,
        });
    }
    Ok(attributes)
}

fn open_tag_qname(xml: &str, start: usize) -> Result<&str, String> {
    let end = scan_open_tag_end(xml, start)?;
    let bytes = xml.as_bytes();
    let mut cursor = start + 1;
    while cursor < end
        && !bytes[cursor].is_ascii_whitespace()
        && !matches!(bytes[cursor], b'/' | b'>')
    {
        cursor += 1;
    }
    Ok(&xml[start + 1..cursor])
}

fn qname_prefix(qname: &str) -> &str {
    qname
        .rsplit_once(':')
        .map(|(prefix, _)| prefix)
        .unwrap_or("")
}

fn qualify(prefix: &str, local: &str) -> String {
    if prefix.is_empty() {
        local.to_string()
    } else {
        format!("{prefix}:{local}")
    }
}

fn patch_open_tag(
    xml: &str,
    start: usize,
    changes: &[(String, Option<String>)],
) -> Result<String, String> {
    if changes.is_empty() {
        return Ok(xml.to_string());
    }
    let end = scan_open_tag_end(xml, start)?;
    let tag = &xml[start..end];
    let attributes = scan_start_tag_attributes(tag)?;
    let mut output = xml.to_string();
    let mut replacements: Vec<(Range<usize>, String)> = Vec::new();
    let mut additions = Vec::new();
    for (name, value) in changes {
        if let Some(attribute) = attributes.iter().find(|attribute| attribute.name == *name) {
            match value {
                Some(value) if attribute.value != *value => replacements.push((
                    start + attribute.value_range.start..start + attribute.value_range.end,
                    xml_escape_attribute(value),
                )),
                None => replacements.push((
                    start + attribute.full_range.start..start + attribute.full_range.end,
                    String::new(),
                )),
                _ => {}
            }
        } else if let Some(value) = value {
            additions.push(format!(" {name}=\"{}\"", xml_escape_attribute(value)));
        }
    }
    replacements.sort_by(|left, right| right.0.start.cmp(&left.0.start));
    for (range, value) in replacements {
        output.replace_range(range, &value);
    }
    if !additions.is_empty() {
        // Existing replacements can occur before the insertion point; account for their delta by
        // locating the same start tag again in the already modified output.
        let updated_end = scan_open_tag_end(&output, start)?;
        let updated_tag = &output[start..updated_end];
        let insert = if updated_tag
            .as_bytes()
            .get(updated_tag.len().saturating_sub(2))
            == Some(&b'/')
        {
            updated_end - 2
        } else {
            updated_end - 1
        };
        output.insert_str(insert, &additions.concat());
    }
    Ok(output)
}

fn bool_change(
    object: &Map<String, Value>,
    key: &str,
    node: Node<'_, '_>,
    attribute: &str,
    default: bool,
) -> Result<Option<(String, Option<String>)>, String> {
    let Some(value) = object.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(node
            .attribute(attribute)
            .is_some()
            .then(|| (attribute.to_string(), None)));
    }
    let requested = value
        .as_bool()
        .ok_or_else(|| format!("{key} must be boolean or null"))?;
    if parse_bool(node.attribute(attribute), default) == requested {
        Ok(None)
    } else if requested == default {
        Ok(Some((attribute.to_string(), None)))
    } else {
        Ok(Some((
            attribute.to_string(),
            Some(if requested { "1" } else { "0" }.to_string()),
        )))
    }
}

fn u64_change(
    object: &Map<String, Value>,
    key: &str,
    node: Node<'_, '_>,
    attribute: &str,
    default: Option<u64>,
    minimum: Option<u64>,
    maximum: Option<u64>,
    removable: bool,
) -> Result<Option<(String, Option<String>)>, String> {
    let Some(value) = object.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        if !removable {
            return Err(format!("{key} is required and cannot be null"));
        }
        return Ok(node
            .attribute(attribute)
            .is_some()
            .then(|| (attribute.to_string(), None)));
    }
    let requested = value
        .as_u64()
        .ok_or_else(|| format!("{key} must be an unsigned integer or null"))?;
    if minimum.is_some_and(|minimum| requested < minimum)
        || maximum.is_some_and(|maximum| requested > maximum)
    {
        return Err(format!("{key} is outside the supported range"));
    }
    if parse_u64(node.attribute(attribute)).or(default) == Some(requested) {
        Ok(None)
    } else if removable && default == Some(requested) {
        Ok(Some((attribute.to_string(), None)))
    } else {
        Ok(Some((attribute.to_string(), Some(requested.to_string()))))
    }
}

fn string_change(
    object: &Map<String, Value>,
    key: &str,
    node: Node<'_, '_>,
    attribute: &str,
    removable: bool,
    nonempty: bool,
) -> Result<Option<(String, Option<String>)>, String> {
    let Some(value) = object.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        if !removable {
            return Err(format!("{key} is required and cannot be null"));
        }
        return Ok(node
            .attribute(attribute)
            .is_some()
            .then(|| (attribute.to_string(), None)));
    }
    let requested = value
        .as_str()
        .ok_or_else(|| format!("{key} must be a string or null"))?;
    if nonempty && requested.is_empty() {
        return Err(format!("{key} cannot be empty"));
    }
    if node.attribute(attribute) == Some(requested) {
        Ok(None)
    } else {
        Ok(Some((attribute.to_string(), Some(requested.to_string()))))
    }
}

fn operation_payload<'a>(
    operation: &'a Map<String, Value>,
) -> Result<&'a Map<String, Value>, String> {
    operation
        .get("patch")
        .map(|value| {
            value
                .as_object()
                .ok_or_else(|| "operation.patch must be an object".to_string())
        })
        .unwrap_or(Ok(operation))
}

fn operation_name(operation: &Map<String, Value>) -> Result<&str, String> {
    operation
        .get("op")
        .and_then(Value::as_str)
        .ok_or_else(|| "slicer operation has no string op".to_string())
}

fn selector_source_index(operation: &Map<String, Value>) -> Option<usize> {
    operation
        .get("sourceIndex")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
}

fn root_closing_tag_start(xml: &str, root: Node<'_, '_>) -> Result<usize, String> {
    let range = root.range();
    xml[range.clone()]
        .rfind("</")
        .map(|relative| range.start + relative)
        .ok_or_else(|| "root element has no closing tag".to_string())
}

fn insert_child_before_close(
    xml: &str,
    parent: Node<'_, '_>,
    child: &str,
) -> Result<String, String> {
    let insert = root_closing_tag_start(xml, parent)?;
    let mut output = xml.to_string();
    output.insert_str(insert, child);
    Ok(output)
}

fn remove_range(xml: &str, range: Range<usize>) -> String {
    let mut output = xml.to_string();
    output.replace_range(range, "");
    output
}

fn reorder_nodes(xml: &str, nodes: &[Node<'_, '_>], order: &[usize]) -> Result<String, String> {
    if order.len() != nodes.len() {
        return Err("reorder must contain every current source index exactly once".to_string());
    }
    let expected: BTreeSet<usize> = (0..nodes.len()).collect();
    let requested: BTreeSet<usize> = order.iter().copied().collect();
    if requested != expected {
        return Err("reorder is not a permutation of current source indexes".to_string());
    }
    let raw: Vec<String> = nodes
        .iter()
        .map(|node| xml[node.range()].to_string())
        .collect();
    let mut output = xml.to_string();
    for (position, node) in nodes.iter().enumerate().rev() {
        output.replace_range(node.range(), &raw[order[position]]);
    }
    Ok(output)
}

fn operation_order(operation: &Map<String, Value>) -> Result<Vec<usize>, String> {
    operation
        .get("order")
        .and_then(Value::as_array)
        .ok_or_else(|| "reorder operation requires an order array".to_string())?
        .iter()
        .map(|value| {
            value
                .as_u64()
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| "reorder indexes must be unsigned integers".to_string())
        })
        .collect()
}

fn cache_root_changes(
    root: Node<'_, '_>,
    patch: &Map<String, Value>,
) -> Result<Vec<(String, Option<String>)>, String> {
    let mut changes = Vec::new();
    if let Some(change) = string_change(patch, "name", root, "name", false, true)? {
        changes.push(change);
    }
    if let Some(change) = string_change(patch, "sourceName", root, "sourceName", false, true)? {
        changes.push(change);
    }
    Ok(changes)
}

fn tabular_changes(
    tabular: Node<'_, '_>,
    patch: &Map<String, Value>,
) -> Result<Vec<(String, Option<String>)>, String> {
    let mut changes = Vec::new();
    if let Some(change) = u64_change(
        patch,
        "pivotCacheId",
        tabular,
        "pivotCacheId",
        None,
        Some(1),
        None,
        false,
    )? {
        changes.push(change);
    }
    if let Some(change) = string_change(patch, "sortOrder", tabular, "sortOrder", true, true)? {
        let value = change.1.as_deref().unwrap_or("ascending");
        if !matches!(value, "ascending" | "descending") {
            return Err("sortOrder must be ascending or descending".to_string());
        }
        changes.push(change);
    }
    if let Some(change) = bool_change(patch, "customListSort", tabular, "customListSort", true)? {
        changes.push(change);
    }
    if let Some(change) = bool_change(patch, "showMissing", tabular, "showMissing", true)? {
        changes.push(change);
    }
    if let Some(change) = string_change(patch, "crossFilter", tabular, "crossFilter", true, true)? {
        let value = change.1.as_deref().unwrap_or("showItemsWithDataAtTop");
        if !matches!(
            value,
            "none" | "showItemsWithDataAtTop" | "showItemsWithNoData"
        ) {
            return Err("unsupported slicer crossFilter value".to_string());
        }
        changes.push(change);
    }
    Ok(changes)
}

fn table_changes(
    table: Node<'_, '_>,
    patch: &Map<String, Value>,
) -> Result<Vec<(String, Option<String>)>, String> {
    let mut changes = Vec::new();
    for (key, attribute) in [("tableId", "tableId"), ("column", "column")] {
        if let Some(change) = u64_change(patch, key, table, attribute, None, Some(1), None, false)?
        {
            changes.push(change);
        }
    }
    if let Some(change) = string_change(patch, "sortOrder", table, "sortOrder", true, true)? {
        let value = change.1.as_deref().unwrap_or("ascending");
        if !matches!(value, "ascending" | "descending") {
            return Err("sortOrder must be ascending or descending".to_string());
        }
        changes.push(change);
    }
    if let Some(change) = bool_change(patch, "customListSort", table, "customListSort", true)? {
        changes.push(change);
    }
    if let Some(change) = string_change(patch, "crossFilter", table, "crossFilter", true, true)? {
        let value = change.1.as_deref().unwrap_or("showItemsWithDataAtTop");
        if !matches!(
            value,
            "none" | "showItemsWithDataAtTop" | "showItemsWithNoData"
        ) {
            return Err("unsupported table slicer crossFilter value".to_string());
        }
        changes.push(change);
    }
    Ok(changes)
}

fn connection_container<'a, 'input>(
    root: Node<'a, 'input>,
    kind: &str,
) -> Option<Node<'a, 'input>> {
    match kind {
        "pivotTables" | "direct" => direct_child(root, "pivotTables"),
        "x15PivotTables" | "x15" => root.descendants().find(|node| {
            node.is_element()
                && local_name(*node) == "slicerCachePivotTables"
                && node.tag_name().namespace() == Some(X15_NS)
        }),
        _ => None,
    }
}

fn selected_connection<'a, 'input>(
    container: Node<'a, 'input>,
    operation: &Map<String, Value>,
) -> Result<Node<'a, 'input>, String> {
    let pivots: Vec<Node<'a, 'input>> = direct_children(container, "pivotTable").collect();
    if let Some(index) = selector_source_index(operation) {
        return pivots
            .get(index)
            .copied()
            .ok_or_else(|| format!("PivotTable connection sourceIndex {index} is missing"));
    }
    let tab_id = operation.get("tabId").and_then(Value::as_u64);
    let name = operation.get("name").and_then(Value::as_str);
    pivots
        .into_iter()
        .find(|pivot| {
            tab_id.is_none_or(|tab_id| parse_u64(pivot.attribute("tabId")) == Some(tab_id))
                && name.is_none_or(|name| pivot.attribute("name") == Some(name))
        })
        .ok_or_else(|| "PivotTable connection selector did not match".to_string())
}

fn sync_optional_count(
    xml: &str,
    container_local: &str,
    child_local: &str,
) -> Result<String, String> {
    let document = Document::parse(xml).map_err(|error| format!("Slicer XML: {error}"))?;
    let Some(container) = document
        .descendants()
        .find(|node| node.is_element() && local_name(*node) == container_local)
    else {
        return Ok(xml.to_string());
    };
    if container.attribute("count").is_none() {
        return Ok(xml.to_string());
    }
    let count = direct_children(container, child_local).count();
    patch_open_tag(
        xml,
        container.range().start,
        &[("count".to_string(), Some(count.to_string()))],
    )
}

fn apply_connection_operation(xml: &str, operation: &Map<String, Value>) -> Result<String, String> {
    let document = Document::parse(xml).map_err(|error| format!("SlicerCache XML: {error}"))?;
    let root = document.root_element();
    let kind = operation
        .get("container")
        .and_then(Value::as_str)
        .unwrap_or("pivotTables");
    match operation_name(operation)? {
        "update" => {
            let container = connection_container(root, kind)
                .ok_or_else(|| format!("connection container {kind} is missing"))?;
            let pivot = selected_connection(container, operation)?;
            let payload = operation_payload(operation)?;
            let mut changes = Vec::new();
            if let Some(change) =
                u64_change(payload, "tabId", pivot, "tabId", None, Some(1), None, false)?
            {
                changes.push(change);
            }
            if let Some(change) = string_change(payload, "name", pivot, "name", false, true)? {
                changes.push(change);
            }
            patch_open_tag(xml, pivot.range().start, &changes)
        }
        "delete" => {
            let container = connection_container(root, kind)
                .ok_or_else(|| format!("connection container {kind} is missing"))?;
            let pivots: Vec<Node<'_, '_>> = direct_children(container, "pivotTable").collect();
            let pivot = selected_connection(container, operation)?;
            if pivots.len() == 1 {
                Ok(remove_range(xml, container.range()))
            } else {
                Ok(remove_range(xml, pivot.range()))
            }
        }
        "add" => {
            let payload = operation_payload(operation)?;
            let tab_id = payload
                .get("tabId")
                .and_then(Value::as_u64)
                .filter(|value| *value > 0)
                .ok_or_else(|| "connection add requires positive tabId".to_string())?;
            let name = payload
                .get("name")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "connection add requires name".to_string())?;
            if let Some(container) = connection_container(root, kind) {
                if direct_children(container, "pivotTable").any(|pivot| {
                    parse_u64(pivot.attribute("tabId")) == Some(tab_id)
                        && pivot.attribute("name") == Some(name)
                }) {
                    return Err(format!("duplicate PivotTable connection {tab_id}:{name}"));
                }
                let prefix = qname_prefix(open_tag_qname(xml, container.range().start)?);
                let qname = qualify(prefix, "pivotTable");
                let child = format!(
                    "<{qname} tabId=\"{tab_id}\" name=\"{}\"/>",
                    xml_escape_attribute(name)
                );
                insert_child_before_close(xml, container, &child)
            } else if matches!(kind, "pivotTables" | "direct") {
                let prefix = qname_prefix(open_tag_qname(xml, root.range().start)?);
                let container_qname = qualify(prefix, "pivotTables");
                let child_qname = qualify(prefix, "pivotTable");
                let raw = format!(
                    "<{container_qname}><{child_qname} tabId=\"{tab_id}\" name=\"{}\"/></{container_qname}>",
                    xml_escape_attribute(name)
                );
                let insert = direct_child(root, "data")
                    .or_else(|| direct_child(root, "extLst"))
                    .map(|node| node.range().start)
                    .unwrap_or(root_closing_tag_start(xml, root)?);
                let mut output = xml.to_string();
                output.insert_str(insert, &raw);
                Ok(output)
            } else {
                Err("an x15 connection can only be added to an existing x15 container".to_string())
            }
        }
        "reorder" => {
            let container = connection_container(root, kind)
                .ok_or_else(|| format!("connection container {kind} is missing"))?;
            let pivots: Vec<Node<'_, '_>> = direct_children(container, "pivotTable").collect();
            reorder_nodes(xml, &pivots, &operation_order(operation)?)
        }
        other => Err(format!("unsupported connection operation {other}")),
    }
}

fn selected_item<'a, 'input>(
    items: Node<'a, 'input>,
    operation: &Map<String, Value>,
) -> Result<Node<'a, 'input>, String> {
    let nodes: Vec<Node<'a, 'input>> = direct_children(items, "i").collect();
    if let Some(index) = selector_source_index(operation) {
        return nodes
            .get(index)
            .copied()
            .ok_or_else(|| format!("slicer item sourceIndex {index} is missing"));
    }
    let item_index = operation
        .get("itemIndex")
        .or_else(|| operation.get("x"))
        .and_then(Value::as_u64)
        .ok_or_else(|| "item update requires sourceIndex or itemIndex".to_string())?;
    nodes
        .into_iter()
        .find(|item| parse_u64(item.attribute("x")) == Some(item_index))
        .ok_or_else(|| format!("slicer item x={item_index} is missing"))
}

fn apply_item_operation(xml: &str, operation: &Map<String, Value>) -> Result<String, String> {
    if operation_name(operation)? != "update" {
        return Err(
            "cache item membership/order is owned by the PivotCache; only update is safe"
                .to_string(),
        );
    }
    let document = Document::parse(xml).map_err(|error| format!("SlicerCache XML: {error}"))?;
    let items = document
        .descendants()
        .find(|node| node.is_element() && local_name(*node) == "items")
        .ok_or_else(|| "tabular slicer cache has no items".to_string())?;
    let item = selected_item(items, operation)?;
    let payload = operation_payload(operation)?;
    let mut changes = Vec::new();
    if let Some(change) = bool_change(payload, "selected", item, "s", false)? {
        changes.push(change);
    }
    if let Some(change) = bool_change(payload, "noData", item, "nd", false)? {
        changes.push(change);
    }
    patch_open_tag(xml, item.range().start, &changes)
}

fn apply_selected_item_indexes(xml: &str, selected: &Value) -> Result<String, String> {
    let requested_array = selected
        .as_array()
        .ok_or_else(|| "selectedItemIndexes must be an array".to_string())?;
    if requested_array.is_empty() {
        return Err("a tabular slicer cache must retain at least one selected item".to_string());
    }
    let requested: BTreeSet<u64> = requested_array
        .iter()
        .map(|value| {
            value
                .as_u64()
                .ok_or_else(|| "selectedItemIndexes entries must be unsigned integers".to_string())
        })
        .collect::<Result<_, _>>()?;
    let document = Document::parse(xml).map_err(|error| format!("SlicerCache XML: {error}"))?;
    let items = document
        .descendants()
        .find(|node| node.is_element() && local_name(*node) == "items")
        .ok_or_else(|| "tabular slicer cache has no items".to_string())?;
    let nodes: Vec<Node<'_, '_>> = direct_children(items, "i").collect();
    let available: BTreeSet<u64> = nodes
        .iter()
        .filter_map(|item| parse_u64(item.attribute("x")))
        .collect();
    if let Some(missing) = requested.difference(&available).next() {
        return Err(format!("selected slicer item x={missing} is missing"));
    }
    let mut edits: Vec<(usize, Option<String>)> = nodes
        .iter()
        .filter_map(|item| {
            let item_index = parse_u64(item.attribute("x"))?;
            let desired = requested.contains(&item_index);
            (parse_bool(item.attribute("s"), false) != desired)
                .then(|| (item.range().start, desired.then(|| "1".to_string())))
        })
        .collect();
    edits.sort_by(|left, right| right.0.cmp(&left.0));
    let mut output = xml.to_string();
    for (start, value) in edits {
        output = patch_open_tag(&output, start, &[("s".to_string(), value)])?;
    }
    Ok(output)
}

fn selected_olap_selection<'a, 'input>(
    selections: Node<'a, 'input>,
    operation: &Map<String, Value>,
) -> Result<Node<'a, 'input>, String> {
    let nodes: Vec<Node<'a, 'input>> = direct_children(selections, "selection").collect();
    if let Some(index) = selector_source_index(operation) {
        return nodes
            .get(index)
            .copied()
            .ok_or_else(|| format!("OLAP selection sourceIndex {index} is missing"));
    }
    let name = operation
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| "OLAP selection selector requires sourceIndex or name".to_string())?;
    nodes
        .into_iter()
        .find(|selection| selection.attribute("n") == Some(name))
        .ok_or_else(|| format!("OLAP selection {name} is missing"))
}

fn apply_olap_selection_operation(
    xml: &str,
    operation: &Map<String, Value>,
) -> Result<String, String> {
    let document = Document::parse(xml).map_err(|error| format!("SlicerCache XML: {error}"))?;
    let selections = document
        .descendants()
        .find(|node| node.is_element() && local_name(*node) == "selections")
        .ok_or_else(|| "OLAP slicer cache has no selections container".to_string())?;
    match operation_name(operation)? {
        "update" => {
            let selection = selected_olap_selection(selections, operation)?;
            let payload = operation_payload(operation)?;
            let changes = string_change(payload, "name", selection, "n", false, true)?
                .into_iter()
                .collect::<Vec<_>>();
            patch_open_tag(xml, selection.range().start, &changes)
        }
        "delete" => {
            let nodes: Vec<Node<'_, '_>> = direct_children(selections, "selection").collect();
            if nodes.len() <= 1 {
                return Err("an OLAP slicer cache must retain at least one selection".to_string());
            }
            let selection = selected_olap_selection(selections, operation)?;
            let output = remove_range(xml, selection.range());
            sync_optional_count(&output, "selections", "selection")
        }
        "add" => {
            let payload = operation_payload(operation)?;
            let raw = if let Some(raw) = payload.get("rawXml").and_then(Value::as_str) {
                let fragment =
                    Document::parse(raw).map_err(|error| format!("OLAP rawXml: {error}"))?;
                if local_name(fragment.root_element()) != "selection" {
                    return Err("OLAP rawXml root must be selection".to_string());
                }
                raw.to_string()
            } else {
                let name = payload
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                    .ok_or_else(|| "OLAP selection add requires name or rawXml".to_string())?;
                let prefix = qname_prefix(open_tag_qname(xml, selections.range().start)?);
                let selection_qname = qualify(prefix, "selection");
                let parent_qname = qualify(prefix, "p");
                let parents = payload
                    .get("parents")
                    .and_then(Value::as_array)
                    .map(|parents| {
                        parents
                            .iter()
                            .map(|parent| {
                                parent.as_str().map(|parent| {
                                    format!(
                                        "<{parent_qname} n=\"{}\"/>",
                                        xml_escape_attribute(parent)
                                    )
                                })
                            })
                            .collect::<Option<Vec<_>>>()
                    })
                    .flatten()
                    .ok_or_else(|| "OLAP selection parents must be a string array".to_string())?
                    .concat();
                format!(
                    "<{selection_qname} n=\"{}\">{parents}</{selection_qname}>",
                    xml_escape_attribute(name)
                )
            };
            let output = insert_child_before_close(xml, selections, &raw)?;
            sync_optional_count(&output, "selections", "selection")
        }
        "reorder" => {
            let nodes: Vec<Node<'_, '_>> = direct_children(selections, "selection").collect();
            reorder_nodes(xml, &nodes, &operation_order(operation)?)
        }
        other => Err(format!("unsupported OLAP selection operation {other}")),
    }
}

fn object_operations<'a>(
    patch: &'a Map<String, Value>,
    key: &str,
) -> Result<Vec<&'a Map<String, Value>>, String> {
    patch
        .get(key)
        .map(|value| {
            value
                .as_array()
                .ok_or_else(|| format!("{key} must be an array"))?
                .iter()
                .map(|operation| {
                    operation
                        .as_object()
                        .ok_or_else(|| format!("{key} entries must be objects"))
                })
                .collect()
        })
        .unwrap_or_else(|| Ok(Vec::new()))
}

fn validate_connection_uniqueness(xml: &str) -> Result<(), String> {
    let document = Document::parse(xml).map_err(|error| format!("SlicerCache XML: {error}"))?;
    for container in document.descendants().filter(|node| {
        node.is_element() && matches!(local_name(*node), "pivotTables" | "slicerCachePivotTables")
    }) {
        let mut targets = HashSet::new();
        for pivot in direct_children(container, "pivotTable") {
            let target = (
                parse_u64(pivot.attribute("tabId")).unwrap_or(0),
                pivot.attribute("name").unwrap_or("").to_ascii_lowercase(),
            );
            if target.0 == 0 || target.1.is_empty() || !targets.insert(target.clone()) {
                return Err(format!(
                    "duplicate or invalid PivotTable connection {}:{}",
                    target.0, target.1
                ));
            }
        }
    }
    Ok(())
}

/// Applies a differential patch to one native `slicerCacheDefinition` XML part.
///
/// Supported keys are root `name`/`sourceName`, `tabular`, `connectionOperations`,
/// `table`, `itemOperations`, and `olapSelectionOperations`. Cache items intentionally support only
/// selection/no-data updates: adding, deleting, or reordering an item would invalidate its
/// PivotCache index. Connections and OLAP selections support update/add/delete/reorder.
pub(crate) fn apply_slicer_cache_edit(cache_xml: &str, patch: &Value) -> Result<String, String> {
    let patch = patch
        .as_object()
        .ok_or_else(|| "slicer cache patch must be an object".to_string())?;
    let document =
        Document::parse(cache_xml).map_err(|error| format!("SlicerCache XML: {error}"))?;
    let root = document.root_element();
    if local_name(root) != "slicerCacheDefinition" {
        return Err("root is not slicerCacheDefinition".to_string());
    }
    let mut output = patch_open_tag(
        cache_xml,
        root.range().start,
        &cache_root_changes(root, patch)?,
    )?;
    if let Some(tabular_patch) = patch.get("tabular") {
        let tabular_patch = tabular_patch
            .as_object()
            .ok_or_else(|| "tabular patch must be an object".to_string())?;
        let document =
            Document::parse(&output).map_err(|error| format!("SlicerCache XML: {error}"))?;
        let tabular = document
            .descendants()
            .find(|node| node.is_element() && local_name(*node) == "tabular")
            .ok_or_else(|| "cache has no tabular data".to_string())?;
        output = patch_open_tag(
            &output,
            tabular.range().start,
            &tabular_changes(tabular, tabular_patch)?,
        )?;
    }
    if let Some(table_patch) = patch.get("table") {
        let table_patch = table_patch
            .as_object()
            .ok_or_else(|| "table patch must be an object".to_string())?;
        let document =
            Document::parse(&output).map_err(|error| format!("SlicerCache XML: {error}"))?;
        let table = document
            .descendants()
            .find(|node| node.is_element() && local_name(*node) == "tableSlicerCache")
            .ok_or_else(|| "cache has no x15 tableSlicerCache extension".to_string())?;
        output = patch_open_tag(
            &output,
            table.range().start,
            &table_changes(table, table_patch)?,
        )?;
    }
    if let Some(selected) = patch.get("selectedItemIndexes") {
        output = apply_selected_item_indexes(&output, selected)?;
    }
    let connection_operations = object_operations(patch, "connectionOperations")?;
    for operation in &connection_operations {
        output = apply_connection_operation(&output, operation)?;
    }
    if !connection_operations.is_empty() {
        validate_connection_uniqueness(&output)?;
    }
    let item_operations = object_operations(patch, "itemOperations")?;
    for operation in &item_operations {
        output = apply_item_operation(&output, operation)?;
    }
    if !item_operations.is_empty() {
        let document =
            Document::parse(&output).map_err(|error| format!("SlicerCache XML: {error}"))?;
        let selected = document
            .descendants()
            .find(|node| node.is_element() && local_name(*node) == "items")
            .map(|items| {
                direct_children(items, "i")
                    .filter(|item| parse_bool(item.attribute("s"), false))
                    .count()
            })
            .unwrap_or(0);
        if selected == 0 {
            return Err(
                "a tabular slicer cache must retain at least one selected item".to_string(),
            );
        }
    }
    for operation in object_operations(patch, "olapSelectionOperations")? {
        output = apply_olap_selection_operation(&output, operation)?;
    }
    Ok(output)
}

fn selected_slicer<'a, 'input>(
    root: Node<'a, 'input>,
    operation: &Map<String, Value>,
) -> Result<Node<'a, 'input>, String> {
    let slicers: Vec<Node<'a, 'input>> = direct_children(root, "slicer").collect();
    if let Some(index) = selector_source_index(operation) {
        return slicers
            .get(index)
            .copied()
            .ok_or_else(|| format!("slicer sourceIndex {index} is missing"));
    }
    let name = operation
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| "slicer selector requires sourceIndex or name".to_string())?;
    slicers
        .into_iter()
        .find(|slicer| slicer.attribute("name") == Some(name))
        .ok_or_else(|| format!("slicer {name} is missing"))
}

fn slicer_view_changes(
    slicer: Node<'_, '_>,
    patch: &Map<String, Value>,
) -> Result<Vec<(String, Option<String>)>, String> {
    let mut changes = Vec::new();
    for (key, attribute, removable, nonempty) in [
        ("name", "name", false, true),
        ("cache", "cache", false, true),
        ("caption", "caption", true, true),
        ("style", "style", true, true),
    ] {
        if let Some(change) = string_change(patch, key, slicer, attribute, removable, nonempty)? {
            changes.push(change);
        }
    }
    for (key, attribute, default, minimum, maximum, removable) in [
        ("startItem", "startItem", Some(0), Some(0), None, true),
        (
            "columnCount",
            "columnCount",
            Some(1),
            Some(1),
            Some(20_000),
            true,
        ),
        ("level", "level", Some(0), Some(0), None, true),
        ("rowHeight", "rowHeight", None, Some(1), None, false),
    ] {
        if let Some(change) = u64_change(
            patch, key, slicer, attribute, default, minimum, maximum, removable,
        )? {
            changes.push(change);
        }
    }
    if let Some(change) = bool_change(patch, "showCaption", slicer, "showCaption", true)? {
        changes.push(change);
    }
    if let Some(change) = bool_change(patch, "lockedPosition", slicer, "lockedPosition", false)? {
        changes.push(change);
    }
    Ok(changes)
}

fn patch_fragment_open_tag(fragment: &str, patch: &Map<String, Value>) -> Result<String, String> {
    let document = Document::parse(fragment).map_err(|error| format!("slicer rawXml: {error}"))?;
    let root = document.root_element();
    if local_name(root) != "slicer" {
        return Err("slicer rawXml root must be slicer".to_string());
    }
    patch_open_tag(
        fragment,
        root.range().start,
        &slicer_view_changes(root, patch)?,
    )
}

fn apply_slicer_view_operation(
    xml: &str,
    operation: &Map<String, Value>,
) -> Result<String, String> {
    let document = Document::parse(xml).map_err(|error| format!("Slicers XML: {error}"))?;
    let root = document.root_element();
    match operation_name(operation)? {
        "update" => {
            let slicer = selected_slicer(root, operation)?;
            let payload = operation_payload(operation)?;
            patch_open_tag(
                xml,
                slicer.range().start,
                &slicer_view_changes(slicer, payload)?,
            )
        }
        "delete" => {
            if direct_children(root, "slicer").count() <= 1 {
                return Err(
                    "cannot delete the last slicer view without removing its OPC part/relationship"
                        .to_string(),
                );
            }
            let slicer = selected_slicer(root, operation)?;
            Ok(remove_range(xml, slicer.range()))
        }
        "add" => {
            let payload = operation_payload(operation)?;
            let raw = if let Some(raw) = payload.get("rawXml").and_then(Value::as_str) {
                patch_fragment_open_tag(raw, payload)?
            } else if let Some(clone_index) = payload
                .get("cloneSourceIndex")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
            {
                let slicers: Vec<Node<'_, '_>> = direct_children(root, "slicer").collect();
                let source = slicers
                    .get(clone_index)
                    .ok_or_else(|| format!("cloneSourceIndex {clone_index} is missing"))?;
                if source
                    .attributes()
                    .any(|attribute| attribute.name() == "uid")
                {
                    return Err(
                        "a slicer with uid cannot be cloned safely; provide rawXml with a new uid"
                            .to_string(),
                    );
                }
                let fragment = &xml[source.range()];
                patch_open_tag(fragment, 0, &slicer_view_changes(*source, payload)?)?
            } else {
                return Err("slicer add requires rawXml or cloneSourceIndex".to_string());
            };
            let open_end = scan_open_tag_end(&raw, 0)?;
            let new_name = scan_start_tag_attributes(&raw[..open_end])?
                .into_iter()
                .find(|attribute| attribute.name == "name")
                .map(|attribute| attribute.value)
                .ok_or_else(|| "new slicer has no name".to_string())?;
            if direct_children(root, "slicer").any(|slicer| {
                slicer
                    .attribute("name")
                    .is_some_and(|name| name.eq_ignore_ascii_case(&new_name))
            }) {
                return Err(format!("duplicate slicer name {new_name}"));
            }
            insert_child_before_close(xml, root, &raw)
        }
        "reorder" => {
            let slicers: Vec<Node<'_, '_>> = direct_children(root, "slicer").collect();
            reorder_nodes(xml, &slicers, &operation_order(operation)?)
        }
        other => Err(format!("unsupported slicer operation {other}")),
    }
}

fn validate_slicers_part(xml: &str) -> Result<(), String> {
    let document = Document::parse(xml).map_err(|error| format!("Slicers XML: {error}"))?;
    let root = document.root_element();
    let slicers: Vec<Node<'_, '_>> = direct_children(root, "slicer").collect();
    if slicers.is_empty() {
        return Err("a slicers part must contain at least one slicer view".to_string());
    }
    let mut names = HashSet::new();
    let mut uids = HashSet::new();
    let any_uid = slicers.iter().any(|slicer| {
        slicer
            .attributes()
            .any(|attribute| attribute.name() == "uid")
    });
    for slicer in slicers {
        let name = slicer.attribute("name").unwrap_or("");
        if name.is_empty() || !names.insert(name.to_ascii_lowercase()) {
            return Err(format!("duplicate or empty slicer view name {name}"));
        }
        let uid = slicer
            .attributes()
            .find(|attribute| attribute.name() == "uid")
            .map(|attribute| attribute.value());
        if any_uid && uid.is_none() {
            return Err("all slicer views must have uid when any view has uid".to_string());
        }
        if let Some(uid) = uid {
            if !uids.insert(uid.to_ascii_lowercase()) {
                return Err(format!("duplicate slicer uid {uid}"));
            }
        }
    }
    Ok(())
}

/// Applies display/style/cache/identity and structural edits to an existing native slicers part.
/// Existing view nodes are kept byte-exact except for requested attributes. Add accepts either a
/// complete `rawXml` fragment or `cloneSourceIndex`; delete/reorder move original raw ranges.
pub(crate) fn apply_slicer_part_edit(slicer_xml: &str, patch: &Value) -> Result<String, String> {
    let patch = patch
        .as_object()
        .ok_or_else(|| "slicers part patch must be an object".to_string())?;
    let document = Document::parse(slicer_xml).map_err(|error| format!("Slicers XML: {error}"))?;
    if local_name(document.root_element()) != "slicers" {
        return Err("root is not slicers".to_string());
    }
    let mut output = slicer_xml.to_string();
    for operation in object_operations(patch, "operations")? {
        output = apply_slicer_view_operation(&output, operation)?;
    }
    validate_slicers_part(&output)?;
    Ok(output)
}

fn embedded_patch(edit: &Map<String, Value>, wrapper: &str) -> Result<Map<String, Value>, String> {
    if let Some(patch) = edit.get(wrapper) {
        return patch
            .as_object()
            .cloned()
            .ok_or_else(|| format!("{wrapper} must be an object"));
    }
    let mut patch = edit.clone();
    patch.remove("part");
    Ok(patch)
}

fn package_edits<'a>(
    patch: &'a Map<String, Value>,
    key: &str,
) -> Result<Vec<&'a Map<String, Value>>, String> {
    object_operations(patch, key)
}

fn validate_connection_edit(
    cache: &NativeSlicerCache,
    model: &NativeSlicerModel,
    operation: &Map<String, Value>,
) -> Result<(), String> {
    if !matches!(operation_name(operation)?, "add" | "update") {
        return Ok(());
    }
    let payload = operation_payload(operation)?;
    let existing = if operation_name(operation)? == "update" {
        let container = operation
            .get("container")
            .and_then(Value::as_str)
            .unwrap_or("pivotTables");
        if let Some(index) = selector_source_index(operation) {
            cache.connections.iter().find(|connection| {
                connection.container == container && connection.source_index == index
            })
        } else {
            let tab_id = operation.get("tabId").and_then(Value::as_u64);
            let name = operation.get("name").and_then(Value::as_str);
            cache.connections.iter().find(|connection| {
                connection.container == container
                    && tab_id.is_none_or(|value| value == connection.tab_id)
                    && name.is_none_or(|value| value == connection.name)
            })
        }
    } else {
        None
    };
    let tab_id = payload
        .get("tabId")
        .and_then(Value::as_u64)
        .or_else(|| existing.map(|connection| connection.tab_id))
        .ok_or_else(|| "connection edit has no target tabId".to_string())?;
    let name = payload
        .get("name")
        .and_then(Value::as_str)
        .or_else(|| existing.map(|connection| connection.name.as_str()))
        .ok_or_else(|| "connection edit has no target name".to_string())?;
    let target = model
        .pivot_tables
        .iter()
        .find(|target| target.tab_id == tab_id && target.name == name)
        .ok_or_else(|| format!("PivotTable target {tab_id}:{name} does not exist"))?;
    if let (Some(cache_id), Some(target_id)) = (cache.pivot_cache_id, target.pivot_cache_id) {
        if cache_id != target_id {
            return Err(format!(
                "PivotTable target {tab_id}:{name} belongs to pivotCacheId {target_id}, not {cache_id}"
            ));
        }
    }
    Ok(())
}

fn validate_table_cache_edit(
    cache: &NativeSlicerCache,
    model: &NativeSlicerModel,
    patch: &Map<String, Value>,
) -> Result<(), String> {
    let Some(table_patch) = patch.get("table") else {
        return Ok(());
    };
    let table_patch = table_patch
        .as_object()
        .ok_or_else(|| "table patch must be an object".to_string())?;
    let current = cache
        .table
        .as_ref()
        .ok_or_else(|| "cache has no x15 tableSlicerCache target".to_string())?;
    let table_id = table_patch
        .get("tableId")
        .and_then(Value::as_u64)
        .unwrap_or(current.table_id);
    let column = table_patch
        .get("column")
        .and_then(Value::as_u64)
        .unwrap_or(current.column);
    let target = model
        .tables
        .iter()
        .find(|target| target.table_id == table_id)
        .ok_or_else(|| format!("table slicer target tableId {table_id} does not exist"))?;
    if !target
        .columns
        .iter()
        .any(|candidate| candidate.id == column)
    {
        return Err(format!(
            "table slicer target column {column} does not exist in tableId {table_id}"
        ));
    }
    Ok(())
}

fn patch_nodes_with_attribute(
    xml: &str,
    local: &str,
    attribute: &str,
    old: &str,
    new: &str,
    predicate: impl Fn(Node<'_, '_>) -> bool,
) -> Result<String, String> {
    let document = Document::parse(xml).map_err(|error| format!("XML patch: {error}"))?;
    let mut starts: Vec<usize> = document
        .descendants()
        .filter(|node| {
            node.is_element()
                && local_name(*node) == local
                && node.attribute(attribute) == Some(old)
                && predicate(*node)
        })
        .map(|node| node.range().start)
        .collect();
    starts.sort_unstable_by(|left, right| right.cmp(left));
    let mut output = xml.to_string();
    for start in starts {
        output = patch_open_tag(
            &output,
            start,
            &[(attribute.to_string(), Some(new.to_string()))],
        )?;
    }
    Ok(output)
}

fn cascade_cache_rename(
    parts: &mut BTreeMap<String, Vec<u8>>,
    model: &NativeSlicerModel,
    old: &str,
    new: &str,
) -> Result<(), String> {
    for slicer_part in &model.slicer_parts {
        let Some(bytes) = parts.get(&slicer_part.part) else {
            continue;
        };
        let xml = std::str::from_utf8(bytes)
            .map_err(|error| format!("{} UTF-8: {error}", slicer_part.part))?;
        let updated = patch_nodes_with_attribute(xml, "slicer", "cache", old, new, |_| true)?;
        if updated != xml {
            parts.insert(slicer_part.part.clone(), updated.into_bytes());
        }
    }
    let Some(bytes) = parts.get(&model.workbook_part) else {
        return Ok(());
    };
    let xml = std::str::from_utf8(bytes)
        .map_err(|error| format!("{} UTF-8: {error}", model.workbook_part))?;
    let updated = patch_nodes_with_attribute(xml, "definedName", "name", old, new, |node| {
        node.text().is_some_and(|text| text.trim() == "#N/A")
    })?;
    if updated != xml {
        parts.insert(model.workbook_part.clone(), updated.into_bytes());
    }
    Ok(())
}

fn drawing_parts(parts: &BTreeMap<String, Vec<u8>>, content_types: &ContentTypes) -> Vec<String> {
    let mut drawings: BTreeSet<String> =
        parts_with_content_type(content_types, DRAWING_CONTENT_TYPE)
            .into_iter()
            .collect();
    for (part, bytes) in parts {
        if drawings.contains(part) || !part.ends_with(".xml") {
            continue;
        }
        if let Ok(xml) = std::str::from_utf8(bytes) {
            if Document::parse(xml)
                .ok()
                .is_some_and(|document| local_name(document.root_element()) == "wsDr")
            {
                drawings.insert(part.clone());
            }
        }
    }
    drawings.into_iter().collect()
}

fn drawing_anchor_for_slicer<'a, 'input>(node: Node<'a, 'input>) -> Option<Node<'a, 'input>> {
    node.ancestors().find(|ancestor| {
        ancestor.is_element()
            && matches!(
                local_name(*ancestor),
                "twoCellAnchor" | "oneCellAnchor" | "absoluteAnchor"
            )
    })
}

fn rename_drawing_slicer(xml: &str, old: &str, new: &str) -> Result<String, String> {
    let document = Document::parse(xml).map_err(|error| format!("Drawing XML: {error}"))?;
    let mut starts = BTreeSet::new();
    for slicer in document.descendants().filter(|node| {
        node.is_element() && local_name(*node) == "slicer" && node.attribute("name") == Some(old)
    }) {
        if let Some(anchor) = drawing_anchor_for_slicer(slicer) {
            for node in anchor
                .descendants()
                .filter(|node| node.is_element() && node.attribute("name") == Some(old))
            {
                starts.insert(node.range().start);
            }
        } else {
            starts.insert(slicer.range().start);
        }
    }
    let mut output = xml.to_string();
    for start in starts.into_iter().rev() {
        output = patch_open_tag(
            &output,
            start,
            &[("name".to_string(), Some(new.to_string()))],
        )?;
    }
    Ok(output)
}

fn delete_drawing_slicer(xml: &str, name: &str) -> Result<String, String> {
    let document = Document::parse(xml).map_err(|error| format!("Drawing XML: {error}"))?;
    let mut ranges: Vec<Range<usize>> = document
        .descendants()
        .filter(|node| {
            node.is_element()
                && local_name(*node) == "slicer"
                && node.attribute("name") == Some(name)
        })
        .filter_map(drawing_anchor_for_slicer)
        .map(|anchor| anchor.range())
        .collect();
    ranges.sort_by(|left, right| right.start.cmp(&left.start));
    ranges.dedup_by(|left, right| left.start == right.start && left.end == right.end);
    let mut output = xml.to_string();
    for range in ranges {
        output.replace_range(range, "");
    }
    Ok(output)
}

fn cascade_drawing_change(
    parts: &mut BTreeMap<String, Vec<u8>>,
    content_types: &ContentTypes,
    old: &str,
    new: Option<&str>,
) -> Result<(), String> {
    for part in drawing_parts(parts, content_types) {
        let Some(bytes) = parts.get(&part) else {
            continue;
        };
        let xml = std::str::from_utf8(bytes).map_err(|error| format!("{part} UTF-8: {error}"))?;
        let updated = if let Some(new) = new {
            rename_drawing_slicer(xml, old, new)?
        } else {
            delete_drawing_slicer(xml, old)?
        };
        if updated != xml {
            parts.insert(part, updated.into_bytes());
        }
    }
    Ok(())
}

fn validate_view_cache_targets(
    parts: &BTreeMap<String, Vec<u8>>,
    model: &NativeSlicerModel,
) -> Result<(), String> {
    let cache_names: HashSet<String> = model
        .caches
        .iter()
        .map(|cache| cache.name.to_ascii_lowercase())
        .collect();
    for part in &model.slicer_parts {
        let (_, document) = parse_xml_part(parts, &part.part)?;
        for slicer in direct_children(document.root_element(), "slicer") {
            let cache = slicer.attribute("cache").unwrap_or("");
            if !cache_names.contains(&cache.to_ascii_lowercase()) {
                return Err(format!(
                    "slicer {} references missing cache {cache}",
                    slicer.attribute("name").unwrap_or("")
                ));
            }
        }
    }
    Ok(())
}

fn ensure_unique_native_names(model: &NativeSlicerModel) -> Result<(), String> {
    let mut caches = HashSet::new();
    for cache in &model.caches {
        if !caches.insert(cache.name.to_ascii_lowercase()) {
            return Err(format!("duplicate slicer cache name {}", cache.name));
        }
    }
    let mut slicers = HashSet::new();
    for part in &model.slicer_parts {
        for slicer in &part.slicers {
            if !slicers.insert(slicer.name.to_ascii_lowercase()) {
                return Err(format!("duplicate slicer view name {}", slicer.name));
            }
        }
    }
    Ok(())
}

/// Atomically applies slicer cache/view edits across an OPC part map.
///
/// Patch shape:
/// `{ "cacheEdits": [{"part": "...", "patch": {...}}],
///    "slicerPartEdits": [{"part": "...", "patch": {"operations": [...]}}] }`.
/// `patch` wrappers are optional; fields can sit beside `part`. Connection targets are checked
/// against real PivotTable relationships and internal x14 `pivotCacheId` values. Cache renames
/// cascade to slicer views and `#N/A` defined names. Slicer view renames/deletes cascade to the
/// associated DrawingML anchors. The caller sees no partial result when any validation fails.
pub(crate) fn apply_slicer_package_edit(
    parts: &mut BTreeMap<String, Vec<u8>>,
    patch: &Value,
) -> Result<NativeSlicerModel, String> {
    let patch = patch
        .as_object()
        .ok_or_else(|| "slicer package patch must be an object".to_string())?;
    let original_model = inspect_native_slicers(parts)?;
    let content_types = parse_content_types(parts)?;
    let cache_edits = package_edits(patch, "cacheEdits")?;
    let slicer_edits = package_edits(patch, "slicerPartEdits")?;
    if cache_edits.is_empty() && slicer_edits.is_empty() {
        return Ok(original_model);
    }
    let mut output = parts.clone();
    let mut cache_renames = Vec::new();
    let mut touched_cache_parts = HashSet::new();
    for edit in cache_edits {
        let part = edit
            .get("part")
            .and_then(Value::as_str)
            .ok_or_else(|| "cache edit requires part".to_string())?;
        let cache = original_model
            .caches
            .iter()
            .find(|cache| cache.part == part)
            .ok_or_else(|| format!("{part} is not a known slicer cache part"))?;
        let cache_patch = embedded_patch(edit, "patch")?;
        validate_table_cache_edit(cache, &original_model, &cache_patch)?;
        let mut validation_cache = cache.clone();
        if let Some(pivot_cache_id) = cache_patch
            .get("tabular")
            .and_then(Value::as_object)
            .and_then(|tabular| tabular.get("pivotCacheId"))
            .and_then(Value::as_u64)
        {
            validation_cache.pivot_cache_id = Some(pivot_cache_id);
        }
        for operation in object_operations(&cache_patch, "connectionOperations")? {
            validate_connection_edit(&validation_cache, &original_model, operation)?;
        }
        let xml = std::str::from_utf8(
            output
                .get(part)
                .ok_or_else(|| format!("missing slicer cache part {part}"))?,
        )
        .map_err(|error| format!("{part} UTF-8: {error}"))?;
        let updated = apply_slicer_cache_edit(xml, &Value::Object(cache_patch.clone()))?;
        let updated_document =
            Document::parse(&updated).map_err(|error| format!("{part} XML: {error}"))?;
        let new_name = updated_document
            .root_element()
            .attribute("name")
            .unwrap_or("")
            .to_string();
        if new_name != cache.name {
            if original_model.caches.iter().any(|candidate| {
                candidate.part != part && candidate.name.eq_ignore_ascii_case(&new_name)
            }) {
                return Err(format!("duplicate slicer cache name {new_name}"));
            }
            cache_renames.push((cache.name.clone(), new_name));
        }
        output.insert(part.to_string(), updated.into_bytes());
        touched_cache_parts.insert(part.to_string());
    }
    for (old, new) in &cache_renames {
        cascade_cache_rename(&mut output, &original_model, old, new)?;
    }

    let mut drawing_renames = Vec::new();
    let mut drawing_deletes = Vec::new();
    for edit in slicer_edits {
        let part = edit
            .get("part")
            .and_then(Value::as_str)
            .ok_or_else(|| "slicer part edit requires part".to_string())?;
        if !original_model
            .slicer_parts
            .iter()
            .any(|candidate| candidate.part == part)
        {
            return Err(format!("{part} is not a known slicers part"));
        }
        let slicer_patch = embedded_patch(edit, "patch")?;
        let mut xml = std::str::from_utf8(
            output
                .get(part)
                .ok_or_else(|| format!("missing slicers part {part}"))?,
        )
        .map_err(|error| format!("{part} UTF-8: {error}"))?
        .to_string();
        for operation in object_operations(&slicer_patch, "operations")? {
            let document = Document::parse(&xml).map_err(|error| format!("{part} XML: {error}"))?;
            let root = document.root_element();
            match operation_name(operation)? {
                "update" => {
                    let slicer = selected_slicer(root, operation)?;
                    let old_name = slicer.attribute("name").unwrap_or("").to_string();
                    let payload = operation_payload(operation)?;
                    if let Some(new_name) = payload.get("name").and_then(Value::as_str) {
                        if new_name != old_name {
                            drawing_renames.push((old_name, new_name.to_string()));
                        }
                    }
                }
                "delete" => {
                    let slicer = selected_slicer(root, operation)?;
                    drawing_deletes.push(slicer.attribute("name").unwrap_or("").to_string());
                }
                _ => {}
            }
            xml = apply_slicer_view_operation(&xml, operation)?;
        }
        output.insert(part.to_string(), xml.into_bytes());
    }
    for (old, new) in &drawing_renames {
        cascade_drawing_change(&mut output, &content_types, old, Some(new))?;
    }
    for name in &drawing_deletes {
        cascade_drawing_change(&mut output, &content_types, name, None)?;
    }

    let final_model = inspect_native_slicers(&output)?;
    ensure_unique_native_names(&final_model)?;
    validate_view_cache_targets(&output, &final_model)?;
    for cache in final_model
        .caches
        .iter()
        .filter(|cache| touched_cache_parts.contains(&cache.part))
    {
        for connection in &cache.connections {
            if !connection.valid {
                return Err(format!(
                    "{} references missing PivotTable {}:{}",
                    cache.part, connection.tab_id, connection.name
                ));
            }
            if let (Some(cache_id), Some(target_id)) =
                (cache.pivot_cache_id, connection.target_pivot_cache_id)
            {
                if cache_id != target_id {
                    return Err(format!(
                        "{} connection {}:{} belongs to pivotCacheId {target_id}, not {cache_id}",
                        cache.part, connection.tab_id, connection.name
                    ));
                }
            }
        }
    }
    *parts = output;
    Ok(final_model)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bytes(value: &str) -> Vec<u8> {
        value.as_bytes().to_vec()
    }

    fn fixture() -> BTreeMap<String, Vec<u8>> {
        BTreeMap::from([
            (
                "[Content_Types].xml".to_string(),
                bytes(
                    r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Override PartName="/custom/book.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/custom/cache/pc.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.pivotCacheDefinition+xml"/><Override PartName="/custom/tables/pt-a.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.pivotTable+xml"/><Override PartName="/custom/tables/pt-b.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.pivotTable+xml"/><Override PartName="/custom/slicer-cache/region.xml" ContentType="application/vnd.ms-excel.slicerCache+xml"/><Override PartName="/custom/views/sheet-slicers.xml" ContentType="application/vnd.ms-excel.slicer+xml"/><Override PartName="/custom/art/art.xml" ContentType="application/vnd.openxmlformats-officedocument.drawing+xml"/></Types>"#,
                ),
            ),
            (
                "_rels/.rels".to_string(),
                bytes(
                    r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="office-weird" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="custom/book.xml"/></Relationships>"#,
                ),
            ),
            (
                "custom/book.xml".to_string(),
                bytes(
                    r##"<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:x14="http://schemas.microsoft.com/office/spreadsheetml/2009/9/main"><sheets><sheet name="Pivot" sheetId="2" r:id="sheet-z"/></sheets><definedNames><definedName name="Slicer_Region">#N/A</definedName><definedName name="KeepMe">#N/A</definedName></definedNames><extLst><ext uri="cache"><x14:slicerCaches><x14:slicerCache r:id="cache-z"/></x14:slicerCaches></ext></extLst></workbook>"##,
                ),
            ),
            (
                "custom/_rels/book.xml.rels".to_string(),
                bytes(
                    r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="cache-z" Type="http://schemas.microsoft.com/office/2007/relationships/slicerCache" Target="slicer-cache/region.xml"/><Relationship Id="sheet-z" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="sheets/pivot.xml"/></Relationships>"#,
                ),
            ),
            (
                "custom/sheets/pivot.xml".to_string(),
                bytes(
                    r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheetData/><drawing r:id="draw-z"/><pivotTableParts count="2"><pivotTablePart r:id="pivot-a"/><pivotTablePart r:id="pivot-b"/></pivotTableParts><extLst><ext uri="s"><slicerList xmlns="http://schemas.microsoft.com/office/spreadsheetml/2009/9/main"><slicer r:id="slicer-z"/></slicerList></ext></extLst></worksheet>"#,
                ),
            ),
            (
                "custom/sheets/_rels/pivot.xml.rels".to_string(),
                bytes(
                    r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="pivot-a" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotTable" Target="../tables/pt-a.xml"/><Relationship Id="draw-z" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing" Target="../art/art.xml"/><Relationship Id="slicer-z" Type="http://schemas.microsoft.com/office/2007/relationships/slicer" Target="../views/sheet-slicers.xml"/><Relationship Id="pivot-b" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotTable" Target="../tables/pt-b.xml"/></Relationships>"#,
                ),
            ),
            (
                "custom/tables/pt-a.xml".to_string(),
                bytes(
                    r#"<pivotTableDefinition xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" name="SalesPivot" cacheId="5" vendor="a"><location ref="A3:D8"/></pivotTableDefinition>"#,
                ),
            ),
            (
                "custom/tables/pt-b.xml".to_string(),
                bytes(
                    r#"<pivotTableDefinition xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" name="MarginPivot" cacheId="5" vendor="b"><location ref="F3:I8"/></pivotTableDefinition>"#,
                ),
            ),
            (
                "custom/tables/_rels/pt-a.xml.rels".to_string(),
                bytes(
                    r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="pc-a" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotCacheDefinition" Target="../cache/pc.xml"/></Relationships>"#,
                ),
            ),
            (
                "custom/tables/_rels/pt-b.xml.rels".to_string(),
                bytes(
                    r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="pc-b" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotCacheDefinition" Target="../cache/pc.xml"/></Relationships>"#,
                ),
            ),
            (
                "custom/cache/pc.xml".to_string(),
                bytes(
                    r#"<pivotCacheDefinition xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:x14="http://schemas.microsoft.com/office/spreadsheetml/2009/9/main"><cacheSource type="worksheet"/><cacheFields count="2"><cacheField name="Date"><sharedItems><d v="2025-01-01T00:00:00"/></sharedItems></cacheField><cacheField name="Region"><sharedItems count="3"><s v="East"/><s v="West"/><s v="North"/></sharedItems></cacheField></cacheFields><extLst><ext uri="pc"><x14:pivotCacheDefinition pivotCacheId="1986639402" opaque="stay"/></ext></extLst></pivotCacheDefinition>"#,
                ),
            ),
            (
                "custom/slicer-cache/region.xml".to_string(),
                bytes(
                    r#"<?xml version="1.0"?><slicerCacheDefinition xmlns="http://schemas.microsoft.com/office/spreadsheetml/2009/9/main" xmlns:x15="http://schemas.microsoft.com/office/spreadsheetml/2010/11/main" xmlns:v="urn:vendor" name="Slicer_Region" sourceName="Region" v:root="keep"><pivotTables><pivotTable tabId="2" name="SalesPivot" v:id="first"><v:opaque keep="one"/></pivotTable><pivotTable tabId="2" name="MarginPivot" v:id="second"/></pivotTables><data><tabular pivotCacheId="1986639402" v:tab="keep"><items count="3" v:items="keep"><i x="0" s="1" v:i="zero"/><i x="2" s="1"/><i x="1" s="1" nd="1"/></items><extLst><ext uri="tabular-opaque"><v:payload/></ext></extLst></tabular></data><extLst><ext uri="x15-connections"><x15:slicerCachePivotTables><x15:pivotTable tabId="2" name="SalesPivot" v:x="stay"/></x15:slicerCachePivotTables></ext><ext uri="opaque"><v:future value="byte-exact"/></ext></extLst></slicerCacheDefinition>"#,
                ),
            ),
            (
                "custom/views/sheet-slicers.xml".to_string(),
                bytes(
                    r#"<?xml version="1.0"?><slicers xmlns="http://schemas.microsoft.com/office/spreadsheetml/2009/9/main" xmlns:x15="http://schemas.microsoft.com/office/spreadsheetml/2010/11/main" xmlns:v="urn:vendor"><slicer name="RegionSlicer" cache="Slicer_Region" caption="Region Filter" columnCount="2" rowHeight="209550" v:keep="first"><extLst><ext uri="new"><x15:future v:data="keep"/></ext></extLst></slicer><slicer name="RegionMirror" cache="Slicer_Region" caption="Mirror" rowHeight="190500" v:keep="second"/></slicers>"#,
                ),
            ),
            (
                "custom/art/art.xml".to_string(),
                bytes(
                    r#"<xdr:wsDr xmlns:xdr="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:sle="http://schemas.microsoft.com/office/drawing/2010/slicer"><xdr:twoCellAnchor editAs="oneCell" vendor="first"><xdr:from/><xdr:to/><xdr:graphicFrame><xdr:nvGraphicFramePr><xdr:cNvPr id="2" name="RegionSlicer"/></xdr:nvGraphicFramePr><a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/drawing/2010/slicer"><sle:slicer name="RegionSlicer"/></a:graphicData></a:graphic></xdr:graphicFrame><xdr:clientData/></xdr:twoCellAnchor><xdr:twoCellAnchor editAs="oneCell" vendor="second"><xdr:from/><xdr:to/><xdr:graphicFrame><xdr:nvGraphicFramePr><xdr:cNvPr id="3" name="RegionMirror"/></xdr:nvGraphicFramePr><a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/drawing/2010/slicer"><sle:slicer name="RegionMirror"/></a:graphicData></a:graphic></xdr:graphicFrame><xdr:clientData/></xdr:twoCellAnchor></xdr:wsDr>"#,
                ),
            ),
        ])
    }

    #[test]
    fn resolves_real_excel_16_graph_without_part_or_relationship_conventions() {
        let model = inspect_native_slicers(&fixture()).unwrap();
        assert_eq!(model.workbook_part, "custom/book.xml");
        assert_eq!(model.caches.len(), 1);
        let cache = &model.caches[0];
        assert_eq!(cache.part, "custom/slicer-cache/region.xml");
        assert_eq!(cache.relationship_id.as_deref(), Some("cache-z"));
        assert_eq!(cache.name, "Slicer_Region");
        assert_eq!(cache.source_name, "Region");
        assert_eq!(cache.data_kind, "tabular");
        assert_eq!(cache.pivot_cache_id, Some(1_986_639_402));
        assert_eq!(cache.items.len(), 3);
        assert!(cache.items.iter().all(|item| item.selected));
        assert_eq!(
            cache
                .items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            vec!["East", "North", "West"]
        );
        assert_eq!(cache.items[1].value, Value::String("North".to_string()));
        assert!(cache.has_x15_extensions);
        assert_eq!(cache.connections.len(), 3);
        assert!(cache.connections.iter().all(|connection| connection.valid));
        assert!(
            cache
                .connections
                .iter()
                .all(|connection| connection.target_pivot_cache_id == Some(1_986_639_402))
        );

        assert_eq!(model.pivot_tables.len(), 2);
        assert_eq!(model.slicer_parts.len(), 1);
        let slicers = &model.slicer_parts[0];
        assert_eq!(slicers.sheet.as_deref(), Some("Pivot"));
        assert_eq!(slicers.relationship_id.as_deref(), Some("slicer-z"));
        assert_eq!(slicers.slicers[0].caption.as_deref(), Some("Region Filter"));
        assert_eq!(slicers.slicers[0].column_count, 2);
        assert_eq!(
            slicers.slicers[0].cache_part.as_deref(),
            Some("custom/slicer-cache/region.xml")
        );
        assert!(model.warnings.is_empty(), "{:?}", model.warnings);

        let json = parse_slicer_model(&fixture()).unwrap();
        assert_eq!(json["caches"][0]["items"][1]["itemIndex"], 2);
        assert_eq!(json["caches"][0]["items"][1]["label"], "North");
        assert_eq!(json["caches"][0]["items"][1]["value"], "North");
        assert_eq!(json["slicerParts"][0]["sheetId"], 2);
    }

    #[test]
    fn no_op_is_byte_exact_at_part_and_package_levels() {
        let parts = fixture();
        let cache = std::str::from_utf8(&parts["custom/slicer-cache/region.xml"]).unwrap();
        let slicers = std::str::from_utf8(&parts["custom/views/sheet-slicers.xml"]).unwrap();
        assert_eq!(apply_slicer_cache_edit(cache, &json!({})).unwrap(), cache);
        assert_eq!(
            apply_slicer_cache_edit(cache, &json!({"itemOperations":[]})).unwrap(),
            cache
        );
        assert_eq!(
            apply_slicer_part_edit(slicers, &json!({"operations":[]})).unwrap(),
            slicers
        );
        let mut edited = parts.clone();
        let model = apply_slicer_package_edit(&mut edited, &json!({})).unwrap();
        assert_eq!(edited, parts);
        assert_eq!(model, inspect_native_slicers(&parts).unwrap());
    }

    #[test]
    fn edits_excel_display_and_selection_while_preserving_vendor_xml() {
        let parts = fixture();
        let cache = std::str::from_utf8(&parts["custom/slicer-cache/region.xml"]).unwrap();
        let updated_cache = apply_slicer_cache_edit(
            cache,
            &json!({
                "tabular":{"sortOrder":"descending","customListSort":false,"crossFilter":"none"},
                "itemOperations":[{"op":"update","itemIndex":2,"selected":false}]
            }),
        )
        .unwrap();
        assert!(updated_cache.contains("sortOrder=\"descending\""));
        assert!(updated_cache.contains("customListSort=\"0\""));
        assert!(updated_cache.contains("crossFilter=\"none\""));
        let document = Document::parse(&updated_cache).unwrap();
        let item = document
            .descendants()
            .find(|node| {
                node.is_element() && local_name(*node) == "i" && node.attribute("x") == Some("2")
            })
            .unwrap();
        assert!(!parse_bool(item.attribute("s"), false));
        assert!(
            item.attribute("s").is_none(),
            "default false should use Excel's omitted form"
        );
        assert!(updated_cache.contains("v:tab=\"keep\""));
        assert!(updated_cache.contains("<v:payload/>"));
        assert!(updated_cache.contains("<v:future value=\"byte-exact\"/>"));

        let slicers = std::str::from_utf8(&parts["custom/views/sheet-slicers.xml"]).unwrap();
        let updated_view = apply_slicer_part_edit(
            slicers,
            &json!({"operations":[{
                "op":"update","sourceIndex":0,"patch":{
                    "caption":"Territory", "columnCount":3, "showCaption":false,
                    "style":"SlicerStyleLight2", "lockedPosition":true, "rowHeight":220000
                }
            }]}),
        )
        .unwrap();
        let document = Document::parse(&updated_view).unwrap();
        let slicer = direct_child(document.root_element(), "slicer").unwrap();
        assert_eq!(slicer.attribute("caption"), Some("Territory"));
        assert_eq!(slicer.attribute("columnCount"), Some("3"));
        assert_eq!(slicer.attribute("showCaption"), Some("0"));
        assert_eq!(slicer.attribute("style"), Some("SlicerStyleLight2"));
        assert_eq!(slicer.attribute("lockedPosition"), Some("1"));
        assert_eq!(slicer.attribute("rowHeight"), Some("220000"));
        assert_eq!(slicer.attribute(("urn:vendor", "keep")), Some("first"));
        assert!(updated_view.contains("<x15:future v:data=\"keep\"/>"));
    }

    #[test]
    fn bulk_selection_and_x15_table_cache_edits_use_native_default_forms() {
        let parts = fixture();
        let cache = std::str::from_utf8(&parts["custom/slicer-cache/region.xml"]).unwrap();
        let selected = apply_slicer_cache_edit(cache, &json!({"selectedItemIndexes":[2]})).unwrap();
        let document = Document::parse(&selected).unwrap();
        let states: Vec<(u64, bool, bool)> = document
            .descendants()
            .filter(|node| node.is_element() && local_name(*node) == "i")
            .map(|item| {
                (
                    parse_u64(item.attribute("x")).unwrap(),
                    parse_bool(item.attribute("s"), false),
                    item.attribute("s").is_some(),
                )
            })
            .collect();
        assert_eq!(
            states,
            vec![(0, false, false), (2, true, true), (1, false, false)]
        );

        let table_cache = r#"<slicerCacheDefinition xmlns="http://schemas.microsoft.com/office/spreadsheetml/2009/9/main" xmlns:x15="http://schemas.microsoft.com/office/spreadsheetml/2010/11/main" xmlns:v="urn:v" name="Slicer_Table" sourceName="Region"><extLst><ext uri="table"><x15:tableSlicerCache tableId="7" column="3" sortOrder="ascending" customListSort="1" crossFilter="showItemsWithDataAtTop" v:keep="yes"><x15:extLst><x15:ext uri="opaque"><v:x/></x15:ext></x15:extLst></x15:tableSlicerCache></ext></extLst></slicerCacheDefinition>"#;
        let updated = apply_slicer_cache_edit(
            table_cache,
            &json!({"table":{
                "tableId":9,"column":4,"sortOrder":"descending",
                "customListSort":false,"crossFilter":"none"
            }}),
        )
        .unwrap();
        let document = Document::parse(&updated).unwrap();
        let table = document
            .descendants()
            .find(|node| node.is_element() && local_name(*node) == "tableSlicerCache")
            .unwrap();
        assert_eq!(table.attribute("tableId"), Some("9"));
        assert_eq!(table.attribute("column"), Some("4"));
        assert_eq!(table.attribute("sortOrder"), Some("descending"));
        assert_eq!(table.attribute("customListSort"), Some("0"));
        assert_eq!(table.attribute("crossFilter"), Some("none"));
        assert_eq!(table.attribute(("urn:v", "keep")), Some("yes"));
        assert!(updated.contains("<v:x/>"));
    }

    #[test]
    fn connections_update_add_delete_and_reorder_original_raw_nodes() {
        let parts = fixture();
        let cache = std::str::from_utf8(&parts["custom/slicer-cache/region.xml"]).unwrap();
        let reordered = apply_slicer_cache_edit(
            cache,
            &json!({"connectionOperations":[{"op":"reorder","order":[1,0]}]}),
        )
        .unwrap();
        let direct_start = reordered.find("<pivotTables>").unwrap();
        let direct_end = reordered.find("</pivotTables>").unwrap();
        let direct = &reordered[direct_start..direct_end];
        assert!(direct.find("v:id=\"second\"").unwrap() < direct.find("v:id=\"first\"").unwrap());
        assert!(direct.contains("v:id=\"second\""));
        assert!(direct.contains("v:id=\"first\""));
        assert!(direct.contains("<v:opaque keep=\"one\"/>"));
        assert!(reordered.contains("v:x=\"stay\""));

        let structurally_edited = apply_slicer_cache_edit(
            cache,
            &json!({"connectionOperations":[
                {"op":"delete","sourceIndex":1},
                {"op":"update","sourceIndex":0,"patch":{"name":"MarginPivot"}},
                {"op":"add","tabId":2,"name":"SalesPivot"}
            ]}),
        )
        .unwrap();
        assert!(structurally_edited.contains("name=\"MarginPivot\" v:id=\"first\""));
        assert!(structurally_edited.contains("<v:opaque keep=\"one\"/>"));
        assert!(!structurally_edited.contains("v:id=\"second\""));
        assert!(structurally_edited.contains("<pivotTable tabId=\"2\" name=\"SalesPivot\"/>"));

        let x15 = apply_slicer_cache_edit(
            cache,
            &json!({"connectionOperations":[{
                "op":"update","container":"x15PivotTables","sourceIndex":0,
                "patch":{"name":"MarginPivot"}
            }]}),
        )
        .unwrap();
        assert!(x15.contains("<x15:pivotTable tabId=\"2\" name=\"MarginPivot\" v:x=\"stay\"/>"));
    }

    #[test]
    fn view_nodes_clone_reorder_delete_without_reserializing_opaque_children() {
        let parts = fixture();
        let slicers = std::str::from_utf8(&parts["custom/views/sheet-slicers.xml"]).unwrap();
        let updated = apply_slicer_part_edit(
            slicers,
            &json!({"operations":[
                {"op":"add","cloneSourceIndex":0,"name":"RegionClone","caption":"Clone"},
                {"op":"reorder","order":[2,1,0]},
                {"op":"delete","sourceIndex":1}
            ]}),
        )
        .unwrap();
        let document = Document::parse(&updated).unwrap();
        let nodes: Vec<Node<'_, '_>> = direct_children(document.root_element(), "slicer").collect();
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].attribute("name"), Some("RegionClone"));
        assert_eq!(nodes[1].attribute("name"), Some("RegionSlicer"));
        assert_eq!(updated.matches("<x15:future v:data=\"keep\"/>").count(), 2);
        assert!(updated.contains("v:keep=\"first\""));
        assert!(!updated.contains("v:keep=\"second\""));
    }

    #[test]
    fn package_edit_cascades_cache_and_view_identity_into_native_links() {
        let mut parts = fixture();
        let model = apply_slicer_package_edit(
            &mut parts,
            &json!({
                "cacheEdits":[{
                    "part":"custom/slicer-cache/region.xml",
                    "patch":{"name":"Slicer_Territory"}
                }],
                "slicerPartEdits":[{
                    "part":"custom/views/sheet-slicers.xml",
                    "patch":{"operations":[
                        {"op":"update","sourceIndex":0,"patch":{"name":"TerritorySlicer"}},
                        {"op":"delete","sourceIndex":1}
                    ]}
                }]
            }),
        )
        .unwrap();
        assert_eq!(model.caches[0].name, "Slicer_Territory");
        assert_eq!(model.slicer_parts[0].slicers.len(), 1);
        assert_eq!(model.slicer_parts[0].slicers[0].name, "TerritorySlicer");
        assert_eq!(model.slicer_parts[0].slicers[0].cache, "Slicer_Territory");

        let workbook = std::str::from_utf8(&parts["custom/book.xml"]).unwrap();
        assert!(workbook.contains("definedName name=\"Slicer_Territory\""));
        assert!(workbook.contains("definedName name=\"KeepMe\""));
        let drawing = std::str::from_utf8(&parts["custom/art/art.xml"]).unwrap();
        assert!(drawing.contains("name=\"TerritorySlicer\""));
        assert!(!drawing.contains("RegionSlicer"));
        assert!(!drawing.contains("RegionMirror"));
        assert!(drawing.contains("vendor=\"first\""));
        assert!(!drawing.contains("vendor=\"second\""));
    }

    #[test]
    fn package_rejects_nonexistent_or_cross_cache_connection_atomically() {
        let original = fixture();
        let mut parts = original.clone();
        let error = apply_slicer_package_edit(
            &mut parts,
            &json!({"cacheEdits":[{
                "part":"custom/slicer-cache/region.xml",
                "patch":{"connectionOperations":[{
                    "op":"update","sourceIndex":0,"patch":{"tabId":99,"name":"Missing"}
                }]}
            }]}),
        )
        .unwrap_err();
        assert!(error.contains("does not exist"));
        assert_eq!(parts, original);
    }

    #[test]
    fn olap_selection_membership_is_editable_and_unknown_parent_xml_survives() {
        let xml = r#"<slicerCacheDefinition xmlns="http://schemas.microsoft.com/office/spreadsheetml/2009/9/main" xmlns:v="urn:v" name="Olap" sourceName="[Geo]"><data><olap pivotCacheId="77"><levels/><selections count="2" v:keep="yes"><selection n="[US]" v:id="one"><p n="[All]" v:p="keep"/></selection><selection n="[CA]" v:id="two"/></selections></olap></data><extLst><ext uri="opaque"><v:x/></ext></extLst></slicerCacheDefinition>"#;
        let updated = apply_slicer_cache_edit(
            xml,
            &json!({"olapSelectionOperations":[
                {"op":"reorder","order":[1,0]},
                {"op":"update","sourceIndex":0,"patch":{"name":"[Canada]"}},
                {"op":"add","name":"[MX]","parents":["[All]"]},
                {"op":"delete","sourceIndex":1}
            ]}),
        )
        .unwrap();
        assert!(updated.contains("count=\"2\""));
        assert!(updated.contains("n=\"[Canada]\" v:id=\"two\""));
        assert!(updated.contains("<selection n=\"[MX]\"><p n=\"[All]\"/></selection>"));
        assert!(!updated.contains("v:id=\"one\""));
        assert!(updated.contains("v:keep=\"yes\""));
        assert!(updated.contains("<v:x/>"));
    }
}
