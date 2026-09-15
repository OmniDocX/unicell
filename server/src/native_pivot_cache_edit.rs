//! Lossless reader/editor for native OOXML PivotCache definitions.
//!
//! PivotTables, slicers, timelines, cache records, and vendor extensions form a relationship
//! graph that is substantially richer than the small model exposed here.  This module therefore
//! never serialises a new `pivotCacheDefinition`.  It resolves the native graph for inspection
//! and patches only explicitly requested refresh attributes on the existing root start tag.
//! Everything else, including unknown attributes, namespaces, children, and relationship parts,
//! remains byte-for-byte unchanged.

use roxmltree::{Document, Node};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;

const REL_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

const REFRESH_BOOLEAN_ATTRIBUTES: &[&str] = &[
    "refreshOnLoad",
    "enableRefresh",
    "backgroundQuery",
    "saveData",
    "upgradeOnRefresh",
];

#[derive(Clone, Debug)]
struct Relationship {
    id: String,
    kind: String,
    resolved_part: Option<String>,
}

#[derive(Clone, Debug)]
struct PivotTableReference {
    part: String,
    name: Option<String>,
    cache_id: u64,
    cache_part: Option<String>,
    sheet: Option<String>,
    sheet_part: Option<String>,
    relationship_id: Option<String>,
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

fn relationship_id(node: Node<'_, '_>) -> Option<String> {
    node.attributes()
        .find(|attribute| attribute.name() == "id" && attribute.namespace() == Some(REL_NS))
        .map(|attribute| attribute.value().to_string())
        .or_else(|| node.attribute("id").map(str::to_string))
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
    if components.is_empty() {
        None
    } else {
        Some(components.join("/"))
    }
}

fn part_directory(part: &str) -> &str {
    part.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("")
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
    let mut result = HashMap::new();
    for node in document
        .descendants()
        .filter(|node| node.is_element() && local_name(*node) == "Relationship")
    {
        let Some(id) = node.attribute("Id") else {
            continue;
        };
        let Some(target) = node.attribute("Target") else {
            continue;
        };
        let target_mode = node.attribute("TargetMode").map(str::to_string);
        let resolved_part = if target_mode
            .as_deref()
            .map(|mode| mode.eq_ignore_ascii_case("External"))
            .unwrap_or(false)
        {
            None
        } else {
            resolve_relationship_target(owner, target)
        };
        result.insert(
            id.to_string(),
            Relationship {
                id: id.to_string(),
                kind: node.attribute("Type").unwrap_or("").to_string(),
                resolved_part,
            },
        );
    }
    Ok(result)
}

fn office_document_part(parts: &BTreeMap<String, Vec<u8>>) -> Result<String, String> {
    let root_relationships = parse_relationships(parts, "")?;
    if let Some(part) = root_relationships
        .values()
        .find(|relationship| relationship.kind.ends_with("/officeDocument"))
        .and_then(|relationship| relationship.resolved_part.clone())
    {
        if parts.contains_key(&part) {
            return Ok(part);
        }
    }
    if parts.contains_key("xl/workbook.xml") {
        Ok("xl/workbook.xml".to_string())
    } else {
        Err("OPC package has no workbook part".to_string())
    }
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

fn bool_attribute(node: Node<'_, '_>, name: &str) -> Option<bool> {
    match node.attribute(name) {
        Some("1" | "true" | "on") => Some(true),
        Some("0" | "false" | "off") => Some(false),
        _ => None,
    }
}

fn optional_string(value: Option<&str>) -> Value {
    value
        .map(|value| Value::String(value.to_string()))
        .unwrap_or(Value::Null)
}

fn optional_bool(value: Option<bool>) -> Value {
    value.map(Value::Bool).unwrap_or(Value::Null)
}

fn optional_u64(value: Option<u64>) -> Value {
    value
        .map(|value| Value::Number(value.into()))
        .unwrap_or(Value::Null)
}

fn worksheet_parts(
    parts: &BTreeMap<String, Vec<u8>>,
    workbook_part: &str,
    workbook: Node<'_, '_>,
    workbook_relationships: &HashMap<String, Relationship>,
) -> Vec<(String, String)> {
    let mut result = Vec::new();
    for sheet in workbook
        .descendants()
        .filter(|node| node.is_element() && local_name(*node) == "sheet")
    {
        let Some(id) = relationship_id(sheet) else {
            continue;
        };
        let Some(relationship) = workbook_relationships.get(&id) else {
            continue;
        };
        if !relationship.kind.ends_with("/worksheet") {
            continue;
        }
        let Some(part) = relationship.resolved_part.as_ref() else {
            continue;
        };
        if parts.contains_key(part) {
            result.push((
                sheet.attribute("name").unwrap_or("").to_string(),
                part.clone(),
            ));
        }
    }
    if result.is_empty() && workbook_part == "xl/workbook.xml" {
        let mut fallback: Vec<String> = parts
            .keys()
            .filter(|part| part.starts_with("xl/worksheets/") && part.ends_with(".xml"))
            .cloned()
            .collect();
        fallback.sort();
        result.extend(
            fallback
                .into_iter()
                .enumerate()
                .map(|(index, part)| (format!("Sheet{}", index + 1), part)),
        );
    }
    result
}

fn pivot_table_cache_part(
    parts: &BTreeMap<String, Vec<u8>>,
    pivot_table_part: &str,
) -> Result<Option<String>, String> {
    Ok(parse_relationships(parts, pivot_table_part)?
        .values()
        .find(|relationship| relationship.kind.ends_with("/pivotCacheDefinition"))
        .and_then(|relationship| relationship.resolved_part.clone()))
}

fn parse_pivot_table_references(
    parts: &BTreeMap<String, Vec<u8>>,
    workbook_part: &str,
    workbook: Node<'_, '_>,
    workbook_relationships: &HashMap<String, Relationship>,
) -> Result<Vec<PivotTableReference>, String> {
    let mut references = Vec::new();
    let mut referenced_parts = HashSet::new();
    for (sheet_name, sheet_part) in
        worksheet_parts(parts, workbook_part, workbook, workbook_relationships)
    {
        let (_, sheet_document) = parse_xml_part(parts, &sheet_part)?;
        let relationships = parse_relationships(parts, &sheet_part)?;
        for pivot in sheet_document
            .descendants()
            .filter(|node| node.is_element() && local_name(*node) == "pivotTablePart")
        {
            let Some(id) = relationship_id(pivot) else {
                continue;
            };
            let Some(relationship) = relationships.get(&id) else {
                continue;
            };
            if !relationship.kind.ends_with("/pivotTable") {
                continue;
            }
            let Some(part) = relationship.resolved_part.as_ref() else {
                continue;
            };
            let (_, table_document) = parse_xml_part(parts, part)?;
            let root = table_document.root_element();
            if local_name(root) != "pivotTableDefinition" {
                return Err(format!("{part} is not a pivotTableDefinition"));
            }
            let cache_id = root
                .attribute("cacheId")
                .ok_or_else(|| format!("{part} has no cacheId"))?
                .parse::<u64>()
                .map_err(|error| format!("{part} cacheId: {error}"))?;
            references.push(PivotTableReference {
                part: part.clone(),
                name: root.attribute("name").map(str::to_string),
                cache_id,
                cache_part: pivot_table_cache_part(parts, part)?,
                sheet: Some(sheet_name.clone()),
                sheet_part: Some(sheet_part.clone()),
                relationship_id: Some(relationship.id.clone()),
            });
            referenced_parts.insert(part.clone());
        }
    }

    // Retain visibility into a valid but currently detached PivotTable part.  This is useful for
    // diagnostics and avoids silently hiding a cache consumer when a producer used nonstandard
    // worksheet markup.
    for part in parts
        .keys()
        .filter(|part| part.starts_with("xl/pivotTables/") && part.ends_with(".xml"))
    {
        if referenced_parts.contains(part) {
            continue;
        }
        let (_, table_document) = parse_xml_part(parts, part)?;
        let root = table_document.root_element();
        if local_name(root) != "pivotTableDefinition" {
            continue;
        }
        let Some(cache_id) = root
            .attribute("cacheId")
            .and_then(|value| value.parse::<u64>().ok())
        else {
            continue;
        };
        references.push(PivotTableReference {
            part: part.clone(),
            name: root.attribute("name").map(str::to_string),
            cache_id,
            cache_part: pivot_table_cache_part(parts, part)?,
            sheet: None,
            sheet_part: None,
            relationship_id: None,
        });
    }
    references.sort_by(|left, right| {
        left.sheet_part
            .cmp(&right.sheet_part)
            .then_with(|| left.part.cmp(&right.part))
    });
    Ok(references)
}

fn pivot_table_reference_json(reference: &PivotTableReference) -> Value {
    json!({
        "part": reference.part,
        "name": optional_string(reference.name.as_deref()),
        "cacheId": reference.cache_id,
        "cachePart": optional_string(reference.cache_part.as_deref()),
        "sheet": optional_string(reference.sheet.as_deref()),
        "sheetPart": optional_string(reference.sheet_part.as_deref()),
        "relationshipId": optional_string(reference.relationship_id.as_deref()),
    })
}

fn cache_records_part(
    parts: &BTreeMap<String, Vec<u8>>,
    cache_part: &str,
) -> Result<Option<String>, String> {
    Ok(parse_relationships(parts, cache_part)?
        .values()
        .find(|relationship| relationship.kind.ends_with("/pivotCacheRecords"))
        .and_then(|relationship| relationship.resolved_part.clone()))
}

fn cache_source_json(root: Node<'_, '_>) -> Value {
    let Some(source) = direct_child(root, "cacheSource") else {
        return Value::Null;
    };
    let worksheet = direct_child(source, "worksheetSource");
    json!({
        "type": source.attribute("type").unwrap_or(""),
        "connectionId": optional_u64(source.attribute("connectionId").and_then(|value| value.parse::<u64>().ok())),
        "sheet": optional_string(worksheet.and_then(|node| node.attribute("sheet"))),
        "ref": optional_string(worksheet.and_then(|node| node.attribute("ref"))),
        "name": optional_string(worksheet.and_then(|node| node.attribute("name"))),
    })
}

fn cache_refresh_json(root: Node<'_, '_>) -> Value {
    json!({
        "refreshOnLoad": optional_bool(bool_attribute(root, "refreshOnLoad")),
        "enableRefresh": optional_bool(bool_attribute(root, "enableRefresh")),
        "backgroundQuery": optional_bool(bool_attribute(root, "backgroundQuery")),
        "saveData": optional_bool(bool_attribute(root, "saveData")),
        "missingItemsLimit": optional_u64(root.attribute("missingItemsLimit").and_then(|value| value.parse::<u64>().ok())),
        "upgradeOnRefresh": optional_bool(bool_attribute(root, "upgradeOnRefresh")),
    })
}

/// Resolves native PivotCache definitions and their PivotTable consumers from an OPC part map.
///
/// No file-name convention is used for cache or PivotTable parts: all live objects are resolved
/// through package relationships.  The result has the stable shape `{ "caches": [...] }`.
pub(crate) fn parse_pivot_cache_model(parts: &BTreeMap<String, Vec<u8>>) -> Result<Value, String> {
    let workbook_part = office_document_part(parts)?;
    let (_, workbook_document) = parse_xml_part(parts, &workbook_part)?;
    let workbook = workbook_document.root_element();
    let workbook_relationships = parse_relationships(parts, &workbook_part)?;
    let pivot_table_references =
        parse_pivot_table_references(parts, &workbook_part, workbook, &workbook_relationships)?;
    let mut caches = Vec::new();
    let pivot_caches = workbook
        .descendants()
        .find(|node| node.is_element() && local_name(*node) == "pivotCaches");
    for cache in pivot_caches
        .into_iter()
        .flat_map(|container| container.children())
        .filter(|node| node.is_element() && local_name(*node) == "pivotCache")
    {
        let cache_id = cache
            .attribute("cacheId")
            .ok_or("workbook pivotCache has no cacheId")?
            .parse::<u64>()
            .map_err(|error| format!("workbook pivotCache cacheId: {error}"))?;
        let relationship_id = relationship_id(cache)
            .ok_or_else(|| format!("workbook pivotCache {cache_id} has no relationship id"))?;
        let relationship = workbook_relationships
            .get(&relationship_id)
            .ok_or_else(|| {
                format!("workbook pivotCache {cache_id} relationship {relationship_id} is missing")
            })?;
        if !relationship.kind.ends_with("/pivotCacheDefinition") {
            return Err(format!(
                "workbook relationship {relationship_id} is not a pivotCacheDefinition"
            ));
        }
        let cache_part = relationship.resolved_part.as_ref().ok_or_else(|| {
            format!("workbook pivotCache {cache_id} has an external cache relationship")
        })?;
        let (_, cache_document) = parse_xml_part(parts, cache_part)?;
        let root = cache_document.root_element();
        if local_name(root) != "pivotCacheDefinition" {
            return Err(format!("{cache_part} is not a pivotCacheDefinition"));
        }
        let mut references: Vec<&PivotTableReference> = pivot_table_references
            .iter()
            .filter(|reference| {
                reference
                    .cache_part
                    .as_deref()
                    .map(|part| part == cache_part)
                    .unwrap_or(reference.cache_id == cache_id)
            })
            .collect();
        references.sort_by(|left, right| left.part.cmp(&right.part));
        let reference_json: Vec<Value> = references
            .iter()
            .map(|reference| pivot_table_reference_json(reference))
            .collect();
        let field_count = direct_child(root, "cacheFields").and_then(|fields| {
            let actual = fields
                .children()
                .filter(|node| node.is_element() && local_name(*node) == "cacheField")
                .count() as u64;
            if actual > 0 {
                Some(actual)
            } else {
                fields
                    .attribute("count")
                    .and_then(|value| value.parse::<u64>().ok())
            }
        });
        caches.push(json!({
            "cacheId": cache_id,
            "part": cache_part,
            "relationshipId": relationship.id,
            "recordsPart": optional_string(cache_records_part(parts, cache_part)?.as_deref()),
            "fieldCount": optional_u64(field_count),
            "source": cache_source_json(root),
            "refresh": cache_refresh_json(root),
            "pivotTables": reference_json,
            "shared": reference_json.len() > 1,
        }));
    }
    caches.sort_by(|left, right| {
        left["cacheId"]
            .as_u64()
            .cmp(&right["cacheId"].as_u64())
            .then_with(|| left["part"].as_str().cmp(&right["part"].as_str()))
    });
    Ok(json!({ "workbookPart": workbook_part, "caches": caches }))
}

fn root_open_tag_range(xml: &str) -> Result<Range<usize>, String> {
    let document = Document::parse(xml).map_err(|error| format!("PivotCache XML: {error}"))?;
    let root = document.root_element();
    if local_name(root) != "pivotCacheDefinition" {
        return Err("root is not pivotCacheDefinition".to_string());
    }
    let start = root.range().start;
    let bytes = xml.as_bytes();
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
            return Ok(start..cursor + 1);
        }
        cursor += 1;
    }
    Err("pivotCacheDefinition start tag is not closed".to_string())
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
            Some(b'\'') => '\'',
            Some(b'"') => '"',
            _ => return Err("XML attribute value is not quoted".to_string()),
        };
        cursor += 1;
        let value_start = cursor;
        while cursor < bytes.len() && bytes[cursor] != quote as u8 {
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

fn remove_start_tag_attribute(xml: &str, name: &str) -> Result<String, String> {
    let range = root_open_tag_range(xml)?;
    let tag = &xml[range.clone()];
    let Some(attribute) = scan_start_tag_attributes(tag)?
        .into_iter()
        .find(|attribute| attribute.name == name)
    else {
        return Ok(xml.to_string());
    };
    let mut result = xml.to_string();
    result.replace_range(
        range.start + attribute.full_range.start..range.start + attribute.full_range.end,
        "",
    );
    Ok(result)
}

fn set_start_tag_attribute(xml: &str, name: &str, value: &str) -> Result<String, String> {
    let range = root_open_tag_range(xml)?;
    let tag = &xml[range.clone()];
    if let Some(attribute) = scan_start_tag_attributes(tag)?
        .into_iter()
        .find(|attribute| attribute.name == name)
    {
        if attribute.value == value {
            return Ok(xml.to_string());
        }
        let mut result = xml.to_string();
        result.replace_range(
            range.start + attribute.value_range.start..range.start + attribute.value_range.end,
            value,
        );
        return Ok(result);
    }
    let insert = if tag.as_bytes().get(tag.len().saturating_sub(2)) == Some(&b'/') {
        range.end - 2
    } else {
        range.end - 1
    };
    let mut result = xml.to_string();
    result.insert_str(insert, &format!(" {name}=\"{value}\""));
    Ok(result)
}

fn patch_object(patch: &Value) -> Result<&Map<String, Value>, String> {
    let object = patch
        .as_object()
        .ok_or("PivotCache refresh patch must be an object")?;
    if let Some(refresh) = object.get("refresh") {
        if object.len() != 1 {
            return Err("refresh wrapper cannot be mixed with direct patch keys".to_string());
        }
        refresh
            .as_object()
            .ok_or_else(|| "PivotCache refresh patch.refresh must be an object".to_string())
    } else {
        Ok(object)
    }
}

fn parse_existing_bool(value: &str) -> Option<bool> {
    match value {
        "1" | "true" | "on" => Some(true),
        "0" | "false" | "off" => Some(false),
        _ => None,
    }
}

fn attribute_value(xml: &str, name: &str) -> Result<Option<String>, String> {
    let range = root_open_tag_range(xml)?;
    Ok(scan_start_tag_attributes(&xml[range])?
        .into_iter()
        .find(|attribute| attribute.name == name)
        .map(|attribute| attribute.value))
}

/// Applies a refresh-only differential patch to a native `pivotCacheDefinition` XML part.
///
/// Accepted fields are `refreshOnLoad`, `enableRefresh`, `backgroundQuery`, `saveData`,
/// `missingItemsLimit`, and `upgradeOnRefresh`, either directly or below a sole `refresh` key.
/// A JSON `null` removes the explicit attribute; omitted fields are untouched.
pub(crate) fn apply_pivot_cache_refresh_patch(
    cache_xml: &str,
    patch: &Value,
) -> Result<String, String> {
    // Validate the source even for a no-op.  This keeps import errors deterministic rather than
    // deferring them until the first non-empty edit.
    let _ = root_open_tag_range(cache_xml)?;
    let patch = patch_object(patch)?;
    for key in patch.keys() {
        if !REFRESH_BOOLEAN_ATTRIBUTES.contains(&key.as_str()) && key != "missingItemsLimit" {
            return Err(format!("unsupported PivotCache refresh attribute {key}"));
        }
    }
    let mut result = cache_xml.to_string();
    for name in REFRESH_BOOLEAN_ATTRIBUTES {
        let Some(value) = patch.get(*name) else {
            continue;
        };
        if value.is_null() {
            result = remove_start_tag_attribute(&result, name)?;
            continue;
        }
        let requested = value
            .as_bool()
            .ok_or_else(|| format!("PivotCache {name} must be boolean or null"))?;
        let existing = attribute_value(&result, name)?
            .as_deref()
            .and_then(parse_existing_bool);
        if existing == Some(requested) {
            continue;
        }
        result = set_start_tag_attribute(&result, name, if requested { "1" } else { "0" })?;
    }
    if let Some(value) = patch.get("missingItemsLimit") {
        if value.is_null() {
            result = remove_start_tag_attribute(&result, "missingItemsLimit")?;
        } else {
            let requested = value
                .as_u64()
                .filter(|value| *value <= u32::MAX as u64)
                .ok_or("PivotCache missingItemsLimit must be an unsigned 32-bit integer or null")?;
            let existing = attribute_value(&result, "missingItemsLimit")?
                .and_then(|value| value.parse::<u64>().ok());
            if existing != Some(requested) {
                result =
                    set_start_tag_attribute(&result, "missingItemsLimit", &requested.to_string())?;
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_parts() -> BTreeMap<String, Vec<u8>> {
        [
            (
                "_rels/.rels",
                br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="office-z" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#.as_slice(),
            ),
            (
                "xl/workbook.xml",
                br#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="North" sheetId="9" r:id="sheet-any-a"/><sheet name="South" sheetId="17" r:id="sheet-any-b"/></sheets><pivotCaches><pivotCache cacheId="42" r:id="cache-arbitrary-z"/></pivotCaches></workbook>"#.as_slice(),
            ),
            (
                "xl/_rels/workbook.xml.rels",
                br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="sheet-any-a" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/north-data.xml"/><Relationship Id="cache-arbitrary-z" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotCacheDefinition" Target="pivotCache/cache-custom-name.xml"/><Relationship Id="sheet-any-b" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/south-data.xml"/></Relationships>"#.as_slice(),
            ),
            (
                "xl/worksheets/north-data.xml",
                br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheetData/><pivotTableParts count="1"><pivotTablePart r:id="north-pivot-rel"/></pivotTableParts></worksheet>"#.as_slice(),
            ),
            (
                "xl/worksheets/_rels/north-data.xml.rels",
                br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="north-pivot-rel" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotTable" Target="../pivotTables/north-pivot.xml"/></Relationships>"#.as_slice(),
            ),
            (
                "xl/worksheets/south-data.xml",
                br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheetData/><pivotTableParts count="1"><pivotTablePart r:id="south-pivot-rel"/></pivotTableParts></worksheet>"#.as_slice(),
            ),
            (
                "xl/worksheets/_rels/south-data.xml.rels",
                br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="south-pivot-rel" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotTable" Target="../pivotTables/south-pivot.xml"/></Relationships>"#.as_slice(),
            ),
            (
                "xl/pivotTables/north-pivot.xml",
                br#"<pivotTableDefinition xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" name="NorthPivot" cacheId="42"><location ref="A3:D9" firstHeaderRow="1" firstDataRow="2" firstDataCol="1"/></pivotTableDefinition>"#.as_slice(),
            ),
            (
                "xl/pivotTables/_rels/north-pivot.xml.rels",
                br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="north-cache-link" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotCacheDefinition" Target="../pivotCache/cache-custom-name.xml"/></Relationships>"#.as_slice(),
            ),
            (
                "xl/pivotTables/south-pivot.xml",
                br#"<pivotTableDefinition xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" name="SouthPivot" cacheId="42"><location ref="F3:I9" firstHeaderRow="1" firstDataRow="2" firstDataCol="1"/></pivotTableDefinition>"#.as_slice(),
            ),
            (
                "xl/pivotTables/_rels/south-pivot.xml.rels",
                br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="south-cache-link" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotCacheDefinition" Target="../pivotCache/cache-custom-name.xml"/></Relationships>"#.as_slice(),
            ),
            (
                "xl/pivotCache/cache-custom-name.xml",
                br#"<?xml version="1.0" encoding="UTF-8"?><pivotCacheDefinition xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" r:id="records-random" saveData="1" refreshOnLoad="false" enableRefresh="1" backgroundQuery="0" missingItemsLimit="500" upgradeOnRefresh="1" vendor="keep"><cacheSource type="worksheet"><worksheetSource ref="A1:C100" sheet="Raw Data"/></cacheSource><cacheFields count="3"><cacheField name="Region"/><cacheField name="Date"/><cacheField name="Amount"/></cacheFields><extLst><ext uri="opaque"><future keep="byte-exact"/></ext></extLst></pivotCacheDefinition>"#.as_slice(),
            ),
            (
                "xl/pivotCache/_rels/cache-custom-name.xml.rels",
                br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="records-random" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotCacheRecords" Target="records/cache-values.xml"/></Relationships>"#.as_slice(),
            ),
            (
                "xl/pivotCache/records/cache-values.xml",
                br#"<pivotCacheRecords xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="0"/>"#.as_slice(),
            ),
        ]
        .into_iter()
        .map(|(name, bytes)| (name.to_string(), bytes.to_vec()))
        .collect()
    }

    #[test]
    fn resolves_arbitrary_relationship_ids_parts_and_shared_cache() {
        let model = parse_pivot_cache_model(&fixture_parts()).unwrap();
        let caches = model["caches"].as_array().unwrap();
        assert_eq!(caches.len(), 1);
        let cache = &caches[0];
        assert_eq!(cache["cacheId"], 42);
        assert_eq!(cache["relationshipId"], "cache-arbitrary-z");
        assert_eq!(cache["part"], "xl/pivotCache/cache-custom-name.xml");
        assert_eq!(
            cache["recordsPart"],
            "xl/pivotCache/records/cache-values.xml"
        );
        assert_eq!(cache["source"]["type"], "worksheet");
        assert_eq!(cache["source"]["sheet"], "Raw Data");
        assert_eq!(cache["source"]["ref"], "A1:C100");
        assert_eq!(cache["fieldCount"], 3);
        assert_eq!(cache["refresh"]["refreshOnLoad"], false);
        assert_eq!(cache["refresh"]["missingItemsLimit"], 500);
        assert_eq!(cache["shared"], true);
        let tables = cache["pivotTables"].as_array().unwrap();
        assert_eq!(tables.len(), 2);
        assert_eq!(tables[0]["cachePart"], cache["part"]);
        assert_eq!(tables[1]["cachePart"], cache["part"]);
        assert!(tables.iter().any(|table| table["sheet"] == "North"));
        assert!(tables.iter().any(|table| table["sheet"] == "South"));
    }

    #[test]
    fn empty_and_semantically_equal_patches_are_byte_exact_no_ops() {
        let parts = fixture_parts();
        let original = std::str::from_utf8(&parts["xl/pivotCache/cache-custom-name.xml"]).unwrap();
        assert_eq!(
            apply_pivot_cache_refresh_patch(original, &json!({})).unwrap(),
            original
        );
        assert_eq!(
            apply_pivot_cache_refresh_patch(
                original,
                &json!({
                    "refresh": {
                        "saveData": true,
                        "refreshOnLoad": false,
                        "enableRefresh": true,
                        "backgroundQuery": false,
                        "missingItemsLimit": 500,
                        "upgradeOnRefresh": true
                    }
                })
            )
            .unwrap(),
            original
        );
    }

    #[test]
    fn refresh_patch_changes_only_requested_root_attributes() {
        let parts = fixture_parts();
        let original = std::str::from_utf8(&parts["xl/pivotCache/cache-custom-name.xml"]).unwrap();
        let original_tag = root_open_tag_range(original).unwrap();
        let original_suffix = &original[original_tag.end..];
        let edited = apply_pivot_cache_refresh_patch(
            original,
            &json!({
                "refreshOnLoad": true,
                "enableRefresh": null,
                "backgroundQuery": true,
                "saveData": false,
                "missingItemsLimit": 1234,
                "upgradeOnRefresh": false
            }),
        )
        .unwrap();
        let edited_tag = root_open_tag_range(&edited).unwrap();
        assert_eq!(&edited[edited_tag.end..], original_suffix);
        let tag = &edited[edited_tag];
        assert!(tag.contains("refreshOnLoad=\"1\""));
        assert!(!tag.contains("enableRefresh="));
        assert!(tag.contains("backgroundQuery=\"1\""));
        assert!(tag.contains("saveData=\"0\""));
        assert!(tag.contains("missingItemsLimit=\"1234\""));
        assert!(tag.contains("upgradeOnRefresh=\"0\""));
        assert!(tag.contains("vendor=\"keep\""));
        assert!(edited.contains("<future keep=\"byte-exact\"/>"));
    }

    #[test]
    fn direct_attribute_patch_rejects_unknown_or_wrong_typed_values() {
        let parts = fixture_parts();
        let original = std::str::from_utf8(&parts["xl/pivotCache/cache-custom-name.xml"]).unwrap();
        assert!(apply_pivot_cache_refresh_patch(original, &json!({"recordCount": 9})).is_err());
        assert!(apply_pivot_cache_refresh_patch(original, &json!({"saveData": "yes"})).is_err());
        assert!(
            apply_pivot_cache_refresh_patch(original, &json!({"missingItemsLimit": -1})).is_err()
        );
    }
}
