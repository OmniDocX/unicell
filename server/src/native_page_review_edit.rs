//! Lossless package-level editor for Excel page-layout, print, review and protection data.
//!
//! The rest of UniCell intentionally treats the original OOXML package as the source of truth.
//! This module follows the same rule: existing parts are never serialized through a generic XML
//! writer.  Requested attributes/text nodes are patched in place and all unrelated attributes,
//! namespace declarations, comments, extension lists and vendor payloads remain byte-for-byte
//! intact.  Package edits are transactional; relationship/content-type/XML validation happens on
//! a staged copy before it replaces the caller's package.

use roxmltree::{Document, Node};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;

const MAIN_NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const REL_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const PACKAGE_REL_NS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
const CONTENT_TYPES_NS: &str = "http://schemas.openxmlformats.org/package/2006/content-types";
const THREADED_NS: &str = "http://schemas.microsoft.com/office/spreadsheetml/2018/threadedcomments";

const OFFICE_DOCUMENT_REL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument";
const WORKSHEET_REL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet";
const COMMENTS_REL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments";
const VML_REL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/vmlDrawing";
const THREADED_COMMENTS_REL: &str =
    "http://schemas.microsoft.com/office/2017/10/relationships/threadedComment";
const PERSON_REL: &str = "http://schemas.microsoft.com/office/2017/10/relationships/person";

const COMMENTS_CONTENT_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.spreadsheetml.comments+xml";
const THREADED_COMMENTS_CONTENT_TYPE: &str = "application/vnd.ms-excel.threadedcomments+xml";
const PERSON_CONTENT_TYPE: &str = "application/vnd.ms-excel.person+xml";
const VML_CONTENT_TYPE: &str = "application/vnd.openxmlformats-officedocument.vmlDrawing";

const WORKSHEET_ORDER: &[&str] = &[
    "sheetPr",
    "dimension",
    "sheetViews",
    "sheetFormatPr",
    "cols",
    "sheetData",
    "sheetCalcPr",
    "sheetProtection",
    "protectedRanges",
    "scenarios",
    "autoFilter",
    "sortState",
    "dataConsolidate",
    "customSheetViews",
    "mergeCells",
    "phoneticPr",
    "conditionalFormatting",
    "dataValidations",
    "hyperlinks",
    "printOptions",
    "pageMargins",
    "pageSetup",
    "headerFooter",
    "rowBreaks",
    "colBreaks",
    "customProperties",
    "cellWatches",
    "ignoredErrors",
    "smartTags",
    "drawing",
    "legacyDrawing",
    "legacyDrawingHF",
    "picture",
    "oleObjects",
    "controls",
    "webPublishItems",
    "tableParts",
    "extLst",
];

const WORKBOOK_ORDER: &[&str] = &[
    "fileVersion",
    "fileSharing",
    "workbookPr",
    "workbookProtection",
    "bookViews",
    "sheets",
    "functionGroups",
    "externalReferences",
    "definedNames",
    "calcPr",
    "oleSize",
    "customWorkbookViews",
    "pivotCaches",
    "smartTagPr",
    "smartTagTypes",
    "webPublishing",
    "fileRecoveryPr",
    "webPublishObjects",
    "extLst",
];

const HEADER_FOOTER_ORDER: &[&str] = &[
    "oddHeader",
    "oddFooter",
    "evenHeader",
    "evenFooter",
    "firstHeader",
    "firstFooter",
];

#[derive(Clone, Debug)]
struct Relationship {
    id: String,
    rel_type: String,
    target: String,
    target_mode: Option<String>,
}

#[derive(Clone, Debug)]
struct SheetBinding {
    name: String,
    sheet_id: u32,
    local_sheet_id: usize,
    relationship_id: String,
    part: String,
}

#[derive(Clone, Debug)]
struct AttributeSpan {
    name: String,
    value: String,
    value_range: Range<usize>,
    full_range: Range<usize>,
}

fn local_name<'a, 'input>(node: Node<'a, 'input>) -> &'input str {
    node.tag_name().name()
}

fn direct_child<'a, 'input>(parent: Node<'a, 'input>, name: &str) -> Option<Node<'a, 'input>> {
    parent
        .children()
        .find(|node| node.is_element() && local_name(*node) == name)
}

fn direct_children<'a, 'input>(
    parent: Node<'a, 'input>,
    name: &'a str,
) -> impl Iterator<Item = Node<'a, 'input>> + 'a {
    parent
        .children()
        .filter(move |node| node.is_element() && local_name(*node) == name)
}

fn scan_open_tag_end(xml: &str, start: usize) -> Result<usize, String> {
    let bytes = xml.as_bytes();
    if bytes.get(start) != Some(&b'<') {
        return Err("XML element does not start with '<'".to_string());
    }
    let mut cursor = start + 1;
    let mut quote = None;
    while cursor < bytes.len() {
        match (quote, bytes[cursor]) {
            (Some(current), value) if current == value => quote = None,
            (None, b'\'' | b'\"') => quote = Some(bytes[cursor]),
            (None, b'>') => return Ok(cursor + 1),
            _ => {}
        }
        cursor += 1;
    }
    Err("unterminated XML start tag".to_string())
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
            Some(b'\"') => b'\"',
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

fn xml_escape_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn xml_escape_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
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
    let mut replacements: Vec<(Range<usize>, String)> = Vec::new();
    let mut additions = Vec::new();
    for (name, value) in changes {
        if let Some(attribute) = attributes.iter().find(|attribute| attribute.name == *name) {
            match value {
                Some(value) if attribute.value != xml_escape_attribute(value) => {
                    replacements.push((
                        start + attribute.value_range.start..start + attribute.value_range.end,
                        xml_escape_attribute(value),
                    ))
                }
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
    let mut output = xml.to_string();
    for (range, value) in replacements {
        output.replace_range(range, &value);
    }
    if !additions.is_empty() {
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

fn closing_tag_start(xml: &str, node: Node<'_, '_>) -> Result<usize, String> {
    let range = node.range();
    xml[range.clone()]
        .rfind("</")
        .map(|relative| range.start + relative)
        .ok_or_else(|| format!("{} has no closing tag", local_name(node)))
}

fn insert_child_before_close(
    xml: &str,
    parent: Node<'_, '_>,
    child: &str,
) -> Result<String, String> {
    let open_end = scan_open_tag_end(xml, parent.range().start)?;
    if xml[parent.range().start..open_end]
        .trim_end()
        .ends_with("/>")
    {
        let qname = open_tag_qname(xml, parent.range().start)?.to_string();
        let mut open = xml[parent.range().start..open_end].to_string();
        let slash = open
            .rfind("/>")
            .ok_or_else(|| "self-closing element is malformed".to_string())?;
        open.replace_range(slash..slash + 2, ">");
        let replacement = format!("{open}{child}</{qname}>");
        let mut output = xml.to_string();
        output.replace_range(parent.range(), &replacement);
        return Ok(output);
    }
    let insert = closing_tag_start(xml, parent)?;
    let mut output = xml.to_string();
    output.insert_str(insert, child);
    Ok(output)
}

fn insert_ordered_child(
    xml: &str,
    parent: Node<'_, '_>,
    child_local: &str,
    child: &str,
    order: &[&str],
) -> Result<String, String> {
    let requested = order
        .iter()
        .position(|name| *name == child_local)
        .unwrap_or(order.len());
    if let Some(next) = parent.children().find(|node| {
        node.is_element()
            && order
                .iter()
                .position(|name| *name == local_name(*node))
                .is_some_and(|position| position > requested)
    }) {
        let mut output = xml.to_string();
        output.insert_str(next.range().start, child);
        Ok(output)
    } else {
        insert_child_before_close(xml, parent, child)
    }
}

fn remove_range(xml: &str, range: Range<usize>) -> String {
    let mut output = xml.to_string();
    output.replace_range(range, "");
    output
}

fn element_text_range(xml: &str, node: Node<'_, '_>) -> Result<Range<usize>, String> {
    let open_end = scan_open_tag_end(xml, node.range().start)?;
    let close_start = closing_tag_start(xml, node)?;
    Ok(open_end..close_start)
}

fn set_simple_text(
    xml: &str,
    parent: Node<'_, '_>,
    name: &str,
    value: Option<&str>,
    order: &[&str],
) -> Result<String, String> {
    if let Some(child) = direct_child(parent, name) {
        if let Some(value) = value {
            let range = element_text_range(xml, child)?;
            let escaped = xml_escape_text(value);
            if xml[range.clone()] == escaped {
                Ok(xml.to_string())
            } else {
                let mut output = xml.to_string();
                output.replace_range(range, &escaped);
                Ok(output)
            }
        } else {
            Ok(remove_range(xml, child.range()))
        }
    } else if let Some(value) = value {
        let prefix = qname_prefix(open_tag_qname(xml, parent.range().start)?);
        let qname = qualify(prefix, name);
        insert_ordered_child(
            xml,
            parent,
            name,
            &format!("<{qname}>{}</{qname}>", xml_escape_text(value)),
            order,
        )
    } else {
        Ok(xml.to_string())
    }
}

fn attrs_json(node: Node<'_, '_>) -> Value {
    let mut attributes = Map::new();
    for attribute in node.attributes() {
        let key = attribute
            .namespace()
            .and_then(|namespace| {
                node.lookup_prefix(namespace)
                    .map(|prefix| format!("{prefix}:{}", attribute.name()))
            })
            .unwrap_or_else(|| attribute.name().to_string());
        attributes.insert(key, Value::String(attribute.value().to_string()));
    }
    Value::Object(attributes)
}

fn value_as_xml_attribute(value: &Value, field: &str) -> Result<Option<String>, String> {
    Ok(match value {
        Value::Null => None,
        Value::String(value) => Some(value.clone()),
        Value::Bool(value) => Some(if *value { "1" } else { "0" }.to_string()),
        Value::Number(value) => Some(value.to_string()),
        _ => return Err(format!("{field} must be a string, number, boolean or null")),
    })
}

fn attribute_changes(
    patch: &Map<String, Value>,
    reserved: &[&str],
) -> Result<Vec<(String, Option<String>)>, String> {
    let mut changes = BTreeMap::new();
    if let Some(attributes) = patch.get("attributes") {
        let attributes = attributes
            .as_object()
            .ok_or_else(|| "attributes must be an object".to_string())?;
        for (name, value) in attributes {
            changes.insert(
                name.clone(),
                value_as_xml_attribute(value, &format!("attributes.{name}"))?,
            );
        }
    }
    for (name, value) in patch {
        if name == "attributes" || reserved.contains(&name.as_str()) || name.starts_with('$') {
            continue;
        }
        changes.insert(name.clone(), value_as_xml_attribute(value, name)?);
    }
    Ok(changes.into_iter().collect())
}

fn attrs_fragment(changes: &[(String, Option<String>)]) -> String {
    changes
        .iter()
        .filter_map(|(name, value)| {
            value
                .as_ref()
                .map(|value| format!(" {name}=\"{}\"", xml_escape_attribute(value)))
        })
        .collect::<String>()
}

fn delete_requested(value: &Value) -> bool {
    value.is_null()
        || value
            .get("$delete")
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

fn part_text<'a>(parts: &'a BTreeMap<String, Vec<u8>>, part: &str) -> Result<&'a str, String> {
    let bytes = parts
        .get(part)
        .ok_or_else(|| format!("missing OOXML part {part}"))?;
    std::str::from_utf8(bytes).map_err(|_| format!("{part} is not UTF-8 XML"))
}

fn normalize_part(path: &str) -> String {
    let replaced = path.replace('\\', "/");
    let mut output: Vec<String> = Vec::new();
    for segment in replaced.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                output.pop();
            }
            segment => output.push(segment.to_string()),
        }
    }
    output.join("/")
}

fn part_dir(path: &str) -> &str {
    path.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("")
}

fn resolve_target(source_part: &str, target: &str) -> String {
    if target.starts_with('/') {
        normalize_part(target)
    } else {
        normalize_part(&format!("{}/{}", part_dir(source_part), target))
    }
}

fn relative_target(source_part: &str, target_part: &str) -> String {
    let source: Vec<String> = part_dir(source_part)
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(str::to_string)
        .collect();
    let normalized_target = normalize_part(target_part);
    let target: Vec<String> = normalized_target
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(str::to_string)
        .collect();
    let mut shared = 0usize;
    while shared < source.len() && shared < target.len() && source[shared] == target[shared] {
        shared += 1;
    }
    let mut output = vec!["..".to_string(); source.len().saturating_sub(shared)];
    output.extend(target.into_iter().skip(shared));
    output.join("/")
}

fn rels_part(source_part: &str) -> String {
    if source_part.is_empty() {
        "_rels/.rels".to_string()
    } else {
        let (dir, file) = source_part
            .rsplit_once('/')
            .map(|(dir, file)| (dir, file))
            .unwrap_or(("", source_part));
        if dir.is_empty() {
            format!("_rels/{file}.rels")
        } else {
            format!("{dir}/_rels/{file}.rels")
        }
    }
}

fn parse_relationships(
    parts: &BTreeMap<String, Vec<u8>>,
    source_part: &str,
) -> Result<Vec<Relationship>, String> {
    let rels_path = rels_part(source_part);
    let Some(bytes) = parts.get(&rels_path) else {
        return Ok(Vec::new());
    };
    let xml = std::str::from_utf8(bytes).map_err(|_| format!("{rels_path} is not UTF-8 XML"))?;
    let document = Document::parse(xml).map_err(|error| format!("{rels_path}: {error}"))?;
    let root = document.root_element();
    if local_name(root) != "Relationships" || root.tag_name().namespace() != Some(PACKAGE_REL_NS) {
        return Err(format!("{rels_path} is not an OPC relationships part"));
    }
    Ok(direct_children(root, "Relationship")
        .filter_map(|node| {
            Some(Relationship {
                id: node.attribute("Id")?.to_string(),
                rel_type: node.attribute("Type")?.to_string(),
                target: node.attribute("Target")?.to_string(),
                target_mode: node.attribute("TargetMode").map(str::to_string),
            })
        })
        .collect())
}

fn office_document_part(parts: &BTreeMap<String, Vec<u8>>) -> Result<String, String> {
    let relationship = parse_relationships(parts, "")?
        .into_iter()
        .find(|relationship| relationship.rel_type == OFFICE_DOCUMENT_REL)
        .ok_or_else(|| "package has no officeDocument relationship".to_string())?;
    Ok(resolve_target("", &relationship.target))
}

fn workbook_sheets(
    parts: &BTreeMap<String, Vec<u8>>,
    workbook_part: &str,
) -> Result<Vec<SheetBinding>, String> {
    let xml = part_text(parts, workbook_part)?;
    let document = Document::parse(xml).map_err(|error| format!("{workbook_part}: {error}"))?;
    let relationships = parse_relationships(parts, workbook_part)?;
    let targets: HashMap<&str, &Relationship> = relationships
        .iter()
        .filter(|relationship| relationship.rel_type == WORKSHEET_REL)
        .map(|relationship| (relationship.id.as_str(), relationship))
        .collect();
    let sheets = document
        .descendants()
        .find(|node| node.is_element() && local_name(*node) == "sheets")
        .ok_or_else(|| "workbook has no sheets collection".to_string())?;
    let mut bindings = Vec::new();
    for (local_sheet_id, sheet) in direct_children(sheets, "sheet").enumerate() {
        let relationship_id = sheet
            .attribute((REL_NS, "id"))
            .or_else(|| sheet.attribute("r:id"))
            .ok_or_else(|| "workbook sheet is missing r:id".to_string())?;
        let relationship = targets
            .get(relationship_id)
            .ok_or_else(|| format!("worksheet relationship {relationship_id} is missing"))?;
        bindings.push(SheetBinding {
            name: sheet.attribute("name").unwrap_or_default().to_string(),
            sheet_id: sheet
                .attribute("sheetId")
                .and_then(|value| value.parse().ok())
                .unwrap_or((local_sheet_id + 1) as u32),
            local_sheet_id,
            relationship_id: relationship_id.to_string(),
            part: resolve_target(workbook_part, &relationship.target),
        });
    }
    Ok(bindings)
}

fn relationship_target(
    parts: &BTreeMap<String, Vec<u8>>,
    source_part: &str,
    rel_type: &str,
) -> Result<Option<(String, String)>, String> {
    Ok(parse_relationships(parts, source_part)?
        .into_iter()
        .find(|relationship| relationship.rel_type == rel_type)
        .map(|relationship| {
            (
                relationship.id,
                resolve_target(source_part, &relationship.target),
            )
        }))
}

fn inspect_singleton(root: Node<'_, '_>, name: &str) -> Value {
    direct_child(root, name)
        .map(attrs_json)
        .unwrap_or(Value::Null)
}

fn inspect_breaks(root: Node<'_, '_>, name: &str) -> Value {
    let Some(container) = direct_child(root, name) else {
        return Value::Null;
    };
    json!({
        "attributes": attrs_json(container),
        "items": direct_children(container, "brk").map(attrs_json).collect::<Vec<_>>()
    })
}

fn inspect_header_footer(root: Node<'_, '_>) -> Value {
    let Some(header_footer) = direct_child(root, "headerFooter") else {
        return Value::Null;
    };
    let mut output = Map::new();
    output.insert("attributes".to_string(), attrs_json(header_footer));
    for name in HEADER_FOOTER_ORDER {
        output.insert(
            (*name).to_string(),
            direct_child(header_footer, name)
                .map(|node| Value::String(node.text().unwrap_or_default().to_string()))
                .unwrap_or(Value::Null),
        );
    }
    Value::Object(output)
}

fn inspect_protected_ranges(root: Node<'_, '_>) -> Value {
    let Some(ranges) = direct_child(root, "protectedRanges") else {
        return Value::Array(Vec::new());
    };
    Value::Array(
        direct_children(ranges, "protectedRange")
            .map(attrs_json)
            .collect(),
    )
}

fn rich_text_plain(node: Node<'_, '_>) -> String {
    node.descendants()
        .filter(|node| node.is_element() && local_name(*node) == "t")
        .filter_map(|node| node.text())
        .collect()
}

fn inspect_notes(parts: &BTreeMap<String, Vec<u8>>, sheet_part: &str) -> Result<Value, String> {
    let Some((relationship_id, comments_part)) =
        relationship_target(parts, sheet_part, COMMENTS_REL)?
    else {
        return Ok(json!({"part": null, "relationshipId": null, "items": []}));
    };
    let xml = part_text(parts, &comments_part)?;
    let document = Document::parse(xml).map_err(|error| format!("{comments_part}: {error}"))?;
    let root = document.root_element();
    let authors: Vec<String> = direct_child(root, "authors")
        .map(|container| {
            direct_children(container, "author")
                .map(|author| author.text().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default();
    let mut items = Vec::new();
    if let Some(comment_list) = direct_child(root, "commentList") {
        for comment in direct_children(comment_list, "comment") {
            let text = direct_child(comment, "text");
            let author_id = comment
                .attribute("authorId")
                .and_then(|value| value.parse::<usize>().ok());
            let mut item = Map::new();
            item.insert(
                "ref".to_string(),
                Value::String(comment.attribute("ref").unwrap_or_default().to_string()),
            );
            item.insert(
                "authorId".to_string(),
                author_id
                    .map(|value| Value::Number(value.into()))
                    .unwrap_or(Value::Null),
            );
            item.insert(
                "author".to_string(),
                author_id
                    .and_then(|index| authors.get(index))
                    .map(|value| Value::String(value.clone()))
                    .unwrap_or(Value::Null),
            );
            item.insert(
                "text".to_string(),
                Value::String(text.map(rich_text_plain).unwrap_or_default()),
            );
            item.insert("attributes".to_string(), attrs_json(comment));
            if let Some(text) = text {
                item.insert(
                    "textXml".to_string(),
                    Value::String(xml[text.range()].to_string()),
                );
            }
            items.push(Value::Object(item));
        }
    }
    Ok(json!({
        "part": comments_part,
        "relationshipId": relationship_id,
        "authors": authors,
        "items": items
    }))
}

fn inspect_persons(
    parts: &BTreeMap<String, Vec<u8>>,
    workbook_part: &str,
) -> Result<(Option<String>, HashMap<String, Value>, Vec<Value>), String> {
    let Some((_relationship_id, persons_part)) =
        relationship_target(parts, workbook_part, PERSON_REL)?
    else {
        return Ok((None, HashMap::new(), Vec::new()));
    };
    let xml = part_text(parts, &persons_part)?;
    let document = Document::parse(xml).map_err(|error| format!("{persons_part}: {error}"))?;
    let mut by_id = HashMap::new();
    let mut values = Vec::new();
    for person in document
        .descendants()
        .filter(|node| node.is_element() && local_name(*node) == "person")
    {
        let value = attrs_json(person);
        if let Some(id) = person.attribute("id") {
            by_id.insert(id.to_string(), value.clone());
        }
        values.push(value);
    }
    Ok((Some(persons_part), by_id, values))
}

fn inspect_threaded_comments(
    parts: &BTreeMap<String, Vec<u8>>,
    workbook_part: &str,
    sheet_part: &str,
) -> Result<Value, String> {
    let Some((relationship_id, threaded_part)) =
        relationship_target(parts, sheet_part, THREADED_COMMENTS_REL)?
    else {
        return Ok(json!({"part": null, "relationshipId": null, "items": []}));
    };
    let (_persons_part, persons, _) = inspect_persons(parts, workbook_part)?;
    let xml = part_text(parts, &threaded_part)?;
    let document = Document::parse(xml).map_err(|error| format!("{threaded_part}: {error}"))?;
    let mut items = Vec::new();
    for comment in document
        .descendants()
        .filter(|node| node.is_element() && local_name(*node) == "threadedComment")
    {
        let person_id = comment.attribute("personId").unwrap_or_default();
        let mut item = Map::new();
        item.insert("attributes".to_string(), attrs_json(comment));
        for name in ["ref", "id", "personId", "parentId", "dT"] {
            item.insert(
                name.to_string(),
                comment
                    .attribute(name)
                    .map(|value| Value::String(value.to_string()))
                    .unwrap_or(Value::Null),
            );
        }
        item.insert(
            "text".to_string(),
            Value::String(
                direct_child(comment, "text")
                    .and_then(|node| node.text())
                    .unwrap_or_default()
                    .to_string(),
            ),
        );
        item.insert(
            "person".to_string(),
            persons.get(person_id).cloned().unwrap_or(Value::Null),
        );
        item.insert(
            "mentions".to_string(),
            direct_child(comment, "mentions")
                .map(|container| {
                    Value::Array(
                        direct_children(container, "mention")
                            .map(attrs_json)
                            .collect(),
                    )
                })
                .unwrap_or_else(|| Value::Array(Vec::new())),
        );
        items.push(Value::Object(item));
    }
    Ok(json!({
        "part": threaded_part,
        "relationshipId": relationship_id,
        "items": items
    }))
}

fn inspect_defined_names(workbook_root: Node<'_, '_>, sheets: &[SheetBinding]) -> Value {
    let Some(container) = direct_child(workbook_root, "definedNames") else {
        return Value::Array(Vec::new());
    };
    Value::Array(
        direct_children(container, "definedName")
            .map(|node| {
                let name = node.attribute("name").unwrap_or_default();
                let local_sheet_id = node
                    .attribute("localSheetId")
                    .and_then(|value| value.parse::<usize>().ok());
                json!({
                    "name": name,
                    "kind": match name {
                        "_xlnm.Print_Area" => "printArea",
                        "_xlnm.Print_Titles" => "printTitles",
                        _ => "other",
                    },
                    "localSheetId": local_sheet_id,
                    "sheet": local_sheet_id.and_then(|index| sheets.get(index)).map(|sheet| sheet.name.clone()),
                    "formula": node.text().unwrap_or_default(),
                    "attributes": attrs_json(node),
                })
            })
            .collect(),
    )
}

/// Inspect the package without normalizing or rewriting any source XML.
///
/// The returned JSON is intentionally close to OOXML.  Attribute bags expose every current
/// attribute (including vendor extensions), while convenience fields make the common page/review
/// controls directly usable by the web client.
pub(crate) fn inspect_page_review_model(
    parts: &BTreeMap<String, Vec<u8>>,
) -> Result<Value, String> {
    let workbook_part = office_document_part(parts)?;
    let workbook_xml = part_text(parts, &workbook_part)?;
    let workbook_document =
        Document::parse(workbook_xml).map_err(|error| format!("{workbook_part}: {error}"))?;
    let workbook_root = workbook_document.root_element();
    if local_name(workbook_root) != "workbook" {
        return Err(format!("{workbook_part} is not a SpreadsheetML workbook"));
    }
    let sheets = workbook_sheets(parts, &workbook_part)?;
    let (persons_part, _persons_by_id, persons) = inspect_persons(parts, &workbook_part)?;
    let mut worksheet_models = Vec::new();
    for binding in &sheets {
        let xml = part_text(parts, &binding.part)?;
        let document =
            Document::parse(xml).map_err(|error| format!("{}: {error}", binding.part))?;
        let root = document.root_element();
        if local_name(root) != "worksheet" {
            return Err(format!("{} is not a SpreadsheetML worksheet", binding.part));
        }
        worksheet_models.push(json!({
            "name": binding.name,
            "sheetId": binding.sheet_id,
            "localSheetId": binding.local_sheet_id,
            "part": binding.part,
            "relationshipId": binding.relationship_id,
            "pageMargins": inspect_singleton(root, "pageMargins"),
            "pageSetup": inspect_singleton(root, "pageSetup"),
            "printOptions": inspect_singleton(root, "printOptions"),
            "headerFooter": inspect_header_footer(root),
            "rowBreaks": inspect_breaks(root, "rowBreaks"),
            "colBreaks": inspect_breaks(root, "colBreaks"),
            "sheetProtection": inspect_singleton(root, "sheetProtection"),
            "protectedRanges": inspect_protected_ranges(root),
            "notes": inspect_notes(parts, &binding.part)?,
            "threadedComments": inspect_threaded_comments(parts, &workbook_part, &binding.part)?,
        }));
    }
    Ok(json!({
        "workbookPart": workbook_part,
        "workbookProtection": inspect_singleton(workbook_root, "workbookProtection"),
        "definedNames": inspect_defined_names(workbook_root, &sheets),
        "personsPart": persons_part,
        "persons": persons,
        "worksheets": worksheet_models,
    }))
}

fn patch_singleton(
    xml: &str,
    element_name: &str,
    patch: &Value,
    order: &[&str],
    required_defaults: &[(&str, &str)],
) -> Result<String, String> {
    let document = Document::parse(xml).map_err(|error| error.to_string())?;
    let root = document.root_element();
    if let Some(node) = direct_child(root, element_name) {
        if delete_requested(patch) {
            return Ok(remove_range(xml, node.range()));
        }
        let object = patch
            .as_object()
            .ok_or_else(|| format!("{element_name} must be an object or null"))?;
        let changes = attribute_changes(object, &[])?;
        patch_open_tag(xml, node.range().start, &changes)
    } else {
        if delete_requested(patch) {
            return Ok(xml.to_string());
        }
        let object = patch
            .as_object()
            .ok_or_else(|| format!("{element_name} must be an object or null"))?;
        let mut changes = attribute_changes(object, &[])?;
        for (name, value) in required_defaults {
            if !changes.iter().any(|(candidate, _)| candidate == name) {
                changes.push(((*name).to_string(), Some((*value).to_string())));
            }
        }
        let prefix = qname_prefix(open_tag_qname(xml, root.range().start)?);
        let qname = qualify(prefix, element_name);
        let child = format!("<{qname}{}/>", attrs_fragment(&changes));
        insert_ordered_child(xml, root, element_name, &child, order)
    }
}

fn patch_header_footer(xml: &str, patch: &Value) -> Result<String, String> {
    let document = Document::parse(xml).map_err(|error| error.to_string())?;
    let root = document.root_element();
    let existing = direct_child(root, "headerFooter");
    if delete_requested(patch) {
        return Ok(existing
            .map(|node| remove_range(xml, node.range()))
            .unwrap_or_else(|| xml.to_string()));
    }
    let object = patch
        .as_object()
        .ok_or_else(|| "headerFooter must be an object or null".to_string())?;
    let reserved = HEADER_FOOTER_ORDER;
    let changes = attribute_changes(object, reserved)?;
    let mut output = if let Some(node) = existing {
        patch_open_tag(xml, node.range().start, &changes)?
    } else {
        let prefix = qname_prefix(open_tag_qname(xml, root.range().start)?);
        let qname = qualify(prefix, "headerFooter");
        let child = format!("<{qname}{}/>", attrs_fragment(&changes));
        insert_ordered_child(xml, root, "headerFooter", &child, WORKSHEET_ORDER)?
    };
    for name in HEADER_FOOTER_ORDER {
        let Some(value) = object.get(*name) else {
            continue;
        };
        let requested = if value.is_null() {
            None
        } else {
            Some(
                value
                    .as_str()
                    .ok_or_else(|| format!("headerFooter.{name} must be a string or null"))?,
            )
        };
        let document = Document::parse(&output).map_err(|error| error.to_string())?;
        let container = direct_child(document.root_element(), "headerFooter")
            .ok_or_else(|| "headerFooter insertion failed".to_string())?;
        output = set_simple_text(&output, container, name, requested, HEADER_FOOTER_ORDER)?;
    }
    Ok(output)
}

fn break_id(value: &Value) -> Option<u64> {
    value
        .get("id")
        .or_else(|| value.get("attributes").and_then(|value| value.get("id")))
        .and_then(|value| match value {
            Value::String(value) => value.parse().ok(),
            Value::Number(value) => value.as_u64(),
            _ => None,
        })
}

fn break_attributes(value: &Value) -> Result<Vec<(String, Option<String>)>, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "break item must be an object".to_string())?;
    let changes = attribute_changes(object, &[])?;
    if !changes
        .iter()
        .any(|(name, value)| name == "id" && value.is_some())
    {
        return Err("break item requires id".to_string());
    }
    Ok(changes)
}

fn validate_break_item(element_name: &str, value: &Value) -> Result<(), String> {
    let attributes = break_attributes(value)?;
    let number = |name: &str| {
        attributes
            .iter()
            .find(|(candidate, _)| candidate == name)
            .and_then(|(_, value)| value.as_deref())
            .and_then(|value| value.parse::<u64>().ok())
    };
    let id = number("id").ok_or_else(|| "break id must be an unsigned integer".to_string())?;
    let id_max = if element_name == "rowBreaks" {
        1_048_575
    } else {
        16_383
    };
    if id > id_max {
        return Err(format!("{element_name} id {id} exceeds {id_max}"));
    }
    let coordinate_max = if element_name == "rowBreaks" {
        16_383
    } else {
        1_048_575
    };
    let minimum = number("min").unwrap_or(0);
    let maximum = number("max").unwrap_or(coordinate_max);
    if minimum > maximum || maximum > coordinate_max {
        return Err(format!(
            "{element_name} min/max must satisfy 0 <= min <= max <= {coordinate_max}"
        ));
    }
    Ok(())
}

fn patch_breaks(xml: &str, element_name: &str, patch: &Value) -> Result<String, String> {
    let document = Document::parse(xml).map_err(|error| error.to_string())?;
    let root = document.root_element();
    let existing = direct_child(root, element_name);
    if delete_requested(patch) {
        return Ok(existing
            .map(|node| remove_range(xml, node.range()))
            .unwrap_or_else(|| xml.to_string()));
    }
    let (replace, upsert, delete_ids, root_patch) = if let Some(array) = patch.as_array() {
        (Some(array.clone()), Vec::new(), HashSet::new(), Map::new())
    } else {
        let object = patch
            .as_object()
            .ok_or_else(|| format!("{element_name} must be an array, object or null"))?;
        let replace = object
            .get("items")
            .map(|value| {
                value
                    .as_array()
                    .cloned()
                    .ok_or_else(|| format!("{element_name}.items must be an array"))
            })
            .transpose()?;
        let upsert = object
            .get("upsert")
            .map(|value| {
                value
                    .as_array()
                    .cloned()
                    .ok_or_else(|| format!("{element_name}.upsert must be an array"))
            })
            .transpose()?
            .unwrap_or_default();
        let delete_ids = object
            .get("deleteIds")
            .map(|value| {
                value
                    .as_array()
                    .ok_or_else(|| format!("{element_name}.deleteIds must be an array"))?
                    .iter()
                    .map(|value| {
                        value
                            .as_u64()
                            .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
                            .ok_or_else(|| "break id must be an unsigned integer".to_string())
                    })
                    .collect::<Result<HashSet<_>, _>>()
            })
            .transpose()?
            .unwrap_or_default();
        (replace, upsert, delete_ids, object.clone())
    };

    let replacing = replace.is_some();
    let mut requested = BTreeMap::new();
    if let Some(items) = replace {
        for value in items {
            validate_break_item(element_name, &value)?;
            let id = break_id(&value).ok_or_else(|| "break item requires id".to_string())?;
            if requested.insert(id, value).is_some() {
                return Err(format!("duplicate break id {id}"));
            }
        }
    }
    let mut upserts = upsert
        .into_iter()
        .map(|value| {
            validate_break_item(element_name, &value)?;
            let id = break_id(&value).ok_or_else(|| "break item requires id".to_string())?;
            Ok((id, value))
        })
        .collect::<Result<BTreeMap<_, _>, String>>()?;
    if replacing {
        upserts.extend(requested.clone());
    }

    let mut output = xml.to_string();
    if let Some(container) = existing {
        let mut removals = direct_children(container, "brk")
            .filter(|node| {
                node.attribute("id")
                    .and_then(|value| value.parse::<u64>().ok())
                    .is_some_and(|id| {
                        delete_ids.contains(&id) || (replacing && !requested.contains_key(&id))
                    })
            })
            .map(|node| node.range())
            .collect::<Vec<_>>();
        removals.sort_by(|left, right| right.start.cmp(&left.start));
        for range in removals {
            output = remove_range(&output, range);
        }
        for (id, value) in upserts {
            let document = Document::parse(&output).map_err(|error| error.to_string())?;
            let container = direct_child(document.root_element(), element_name)
                .ok_or_else(|| format!("{element_name} disappeared while patching"))?;
            let existing_item = direct_children(container, "brk").find(|node| {
                node.attribute("id")
                    .and_then(|value| value.parse::<u64>().ok())
                    == Some(id)
            });
            if let Some(node) = existing_item {
                output = patch_open_tag(&output, node.range().start, &break_attributes(&value)?)?;
            } else {
                let prefix = qname_prefix(open_tag_qname(&output, container.range().start)?);
                let qname = qualify(prefix, "brk");
                let fragment = format!("<{qname}{}/>", attrs_fragment(&break_attributes(&value)?));
                output = insert_child_before_close(&output, container, &fragment)?;
            }
        }
        let document = Document::parse(&output).map_err(|error| error.to_string())?;
        let container = direct_child(document.root_element(), element_name)
            .ok_or_else(|| format!("{element_name} disappeared while patching"))?;
        output = patch_open_tag(
            &output,
            container.range().start,
            &attribute_changes(&root_patch, &["items", "upsert", "deleteIds"])?,
        )?;
    } else {
        if upserts.is_empty() {
            return Ok(output);
        }
        let prefix = qname_prefix(open_tag_qname(xml, root.range().start)?);
        let root_qname = qualify(prefix, element_name);
        let item_qname = qualify(prefix, "brk");
        let mut children = String::new();
        for value in upserts.values() {
            children.push_str(&format!(
                "<{item_qname}{}/>",
                attrs_fragment(&break_attributes(value)?)
            ));
        }
        let attributes = attribute_changes(&root_patch, &["items", "upsert", "deleteIds"])?;
        let fragment = format!(
            "<{root_qname}{}>{children}</{root_qname}>",
            attrs_fragment(&attributes)
        );
        output = insert_ordered_child(xml, root, element_name, &fragment, WORKSHEET_ORDER)?;
    }

    let document = Document::parse(&output).map_err(|error| error.to_string())?;
    let container = direct_child(document.root_element(), element_name)
        .ok_or_else(|| format!("{element_name} insertion failed"))?;
    let items = direct_children(container, "brk").collect::<Vec<_>>();
    if items.is_empty() {
        let has_unknown_children = container.children().any(|node| {
            (node.is_element() && local_name(node) != "brk")
                || node.is_comment()
                || node.is_pi()
                || node.text().is_some_and(|text| !text.trim().is_empty())
        });
        if !has_unknown_children {
            return Ok(remove_range(&output, container.range()));
        }
    }
    let manual_count = items
        .iter()
        .filter(|node| {
            node.attribute("man")
                .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        })
        .count();
    patch_open_tag(
        &output,
        container.range().start,
        &[
            ("count".to_string(), Some(items.len().to_string())),
            (
                "manualBreakCount".to_string(),
                Some(manual_count.to_string()),
            ),
        ],
    )
}

fn protected_range_name(value: &Value) -> Option<&str> {
    value
        .get("name")
        .or_else(|| value.get("attributes").and_then(|value| value.get("name")))
        .and_then(Value::as_str)
}

fn patch_protected_ranges(xml: &str, patch: &Value) -> Result<String, String> {
    let document = Document::parse(xml).map_err(|error| error.to_string())?;
    let root = document.root_element();
    let existing = direct_child(root, "protectedRanges");
    if delete_requested(patch) {
        return Ok(existing
            .map(|node| remove_range(xml, node.range()))
            .unwrap_or_else(|| xml.to_string()));
    }
    let (replace, upsert, delete_names, root_patch) = if let Some(items) = patch.as_array() {
        (Some(items.clone()), Vec::new(), HashSet::new(), Map::new())
    } else {
        let object = patch
            .as_object()
            .ok_or_else(|| "protectedRanges must be an array, object or null".to_string())?;
        let replace = object
            .get("items")
            .map(|value| {
                value
                    .as_array()
                    .cloned()
                    .ok_or_else(|| "protectedRanges.items must be an array".to_string())
            })
            .transpose()?;
        let upsert = object
            .get("upsert")
            .map(|value| {
                value
                    .as_array()
                    .cloned()
                    .ok_or_else(|| "protectedRanges.upsert must be an array".to_string())
            })
            .transpose()?
            .unwrap_or_default();
        let delete_names = object
            .get("deleteNames")
            .map(|value| {
                value
                    .as_array()
                    .ok_or_else(|| "protectedRanges.deleteNames must be an array".to_string())?
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .map(str::to_string)
                            .ok_or_else(|| "protected range name must be a string".to_string())
                    })
                    .collect::<Result<HashSet<_>, _>>()
            })
            .transpose()?
            .unwrap_or_default();
        (replace, upsert, delete_names, object.clone())
    };
    let replacing = replace.is_some();
    let mut requested = BTreeMap::new();
    if let Some(items) = replace {
        for item in items {
            let name = protected_range_name(&item)
                .ok_or_else(|| "protected range requires name".to_string())?
                .to_string();
            if requested.insert(name.clone(), item).is_some() {
                return Err(format!("duplicate protected range {name}"));
            }
        }
    }
    let mut upserts = upsert
        .into_iter()
        .map(|item| {
            let name = protected_range_name(&item)
                .ok_or_else(|| "protected range requires name".to_string())?
                .to_string();
            Ok((name, item))
        })
        .collect::<Result<BTreeMap<_, _>, String>>()?;
    if replacing {
        upserts.extend(requested.clone());
    }

    let mut output = xml.to_string();
    if let Some(container) = existing {
        let mut removals = direct_children(container, "protectedRange")
            .filter(|node| {
                node.attribute("name").is_some_and(|name| {
                    delete_names.contains(name) || (replacing && !requested.contains_key(name))
                })
            })
            .map(|node| node.range())
            .collect::<Vec<_>>();
        removals.sort_by(|left, right| right.start.cmp(&left.start));
        for range in removals {
            output = remove_range(&output, range);
        }
        for (name, item) in upserts {
            let object = item
                .as_object()
                .ok_or_else(|| "protected range must be an object".to_string())?;
            let attributes = attribute_changes(object, &[])?;
            let document = Document::parse(&output).map_err(|error| error.to_string())?;
            let container = direct_child(document.root_element(), "protectedRanges")
                .ok_or_else(|| "protectedRanges disappeared while patching".to_string())?;
            let existing_range = direct_children(container, "protectedRange")
                .find(|node| node.attribute("name") == Some(name.as_str()))
                .map(|node| node.range());
            if let Some(range) = existing_range {
                output = patch_open_tag(&output, range.start, &attributes)?;
            } else {
                let has_sqref = attributes
                    .iter()
                    .any(|(name, value)| name == "sqref" && value.is_some());
                if !has_sqref {
                    return Err("new protected range requires sqref".to_string());
                }
                let prefix = qname_prefix(open_tag_qname(&output, container.range().start)?);
                let qname = qualify(prefix, "protectedRange");
                output = insert_child_before_close(
                    &output,
                    container,
                    &format!("<{qname}{}/>", attrs_fragment(&attributes)),
                )?;
            }
        }
        let document = Document::parse(&output).map_err(|error| error.to_string())?;
        let container = direct_child(document.root_element(), "protectedRanges")
            .ok_or_else(|| "protectedRanges disappeared while patching".to_string())?;
        output = patch_open_tag(
            &output,
            container.range().start,
            &attribute_changes(&root_patch, &["items", "upsert", "deleteNames"])?,
        )?;
    } else {
        if upserts.is_empty() {
            return Ok(output);
        }
        let prefix = qname_prefix(open_tag_qname(xml, root.range().start)?);
        let container_qname = qualify(prefix, "protectedRanges");
        let item_qname = qualify(prefix, "protectedRange");
        let mut children = String::new();
        for item in upserts.values() {
            let object = item
                .as_object()
                .ok_or_else(|| "protected range must be an object".to_string())?;
            let attributes = attribute_changes(object, &[])?;
            if !attributes
                .iter()
                .any(|(name, value)| name == "sqref" && value.is_some())
            {
                return Err("new protected range requires sqref".to_string());
            }
            children.push_str(&format!("<{item_qname}{}/>", attrs_fragment(&attributes)));
        }
        let root_attributes = attribute_changes(&root_patch, &["items", "upsert", "deleteNames"])?;
        let fragment = format!(
            "<{container_qname}{}>{children}</{container_qname}>",
            attrs_fragment(&root_attributes)
        );
        output = insert_ordered_child(xml, root, "protectedRanges", &fragment, WORKSHEET_ORDER)?;
    }
    let document = Document::parse(&output).map_err(|error| error.to_string())?;
    let container = direct_child(document.root_element(), "protectedRanges")
        .ok_or_else(|| "protectedRanges insertion failed".to_string())?;
    if direct_children(container, "protectedRange")
        .next()
        .is_none()
    {
        let has_unknown_children = container.children().any(|node| {
            (node.is_element() && local_name(node) != "protectedRange")
                || node.is_comment()
                || node.is_pi()
                || node.text().is_some_and(|text| !text.trim().is_empty())
        });
        if !has_unknown_children {
            return Ok(remove_range(&output, container.range()));
        }
    }
    Ok(output)
}

fn select_sheet<'a>(
    sheets: &'a [SheetBinding],
    target: &Map<String, Value>,
) -> Result<&'a SheetBinding, String> {
    if let Some(part) = target.get("part").and_then(Value::as_str) {
        let part = normalize_part(part);
        return sheets
            .iter()
            .find(|sheet| sheet.part == part)
            .ok_or_else(|| format!("unknown worksheet part {part}"));
    }
    if let Some(sheet_id) = target.get("sheetId").and_then(Value::as_u64) {
        return sheets
            .iter()
            .find(|sheet| sheet.sheet_id as u64 == sheet_id)
            .ok_or_else(|| format!("unknown sheetId {sheet_id}"));
    }
    if let Some(index) = target.get("localSheetId").and_then(Value::as_u64) {
        return sheets
            .get(index as usize)
            .ok_or_else(|| format!("unknown localSheetId {index}"));
    }
    if let Some(name) = target
        .get("sheet")
        .or_else(|| target.get("name"))
        .and_then(Value::as_str)
    {
        return sheets
            .iter()
            .find(|sheet| sheet.name == name)
            .ok_or_else(|| format!("unknown worksheet {name}"));
    }
    if sheets.len() == 1 {
        Ok(&sheets[0])
    } else {
        Err("worksheet edit needs part, sheetId, localSheetId or sheet".to_string())
    }
}

fn defined_name_key(value: &Value, sheets: &[SheetBinding]) -> Result<(String, usize), String> {
    let object = value
        .as_object()
        .ok_or_else(|| "defined-name edit must be an object".to_string())?;
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| "defined-name edit requires name".to_string())?
        .to_string();
    let local_sheet_id = if let Some(index) = object.get("localSheetId").and_then(Value::as_u64) {
        index as usize
    } else if let Some(sheet_id) = object.get("sheetId").and_then(Value::as_u64) {
        sheets
            .iter()
            .find(|sheet| sheet.sheet_id as u64 == sheet_id)
            .map(|sheet| sheet.local_sheet_id)
            .ok_or_else(|| format!("unknown sheetId {sheet_id}"))?
    } else if let Some(name) = object.get("sheet").and_then(Value::as_str) {
        sheets
            .iter()
            .find(|sheet| sheet.name == name)
            .map(|sheet| sheet.local_sheet_id)
            .ok_or_else(|| format!("unknown worksheet {name}"))?
    } else {
        return Err("print defined name requires localSheetId, sheetId or sheet".to_string());
    };
    if local_sheet_id >= sheets.len() {
        return Err(format!("unknown localSheetId {local_sheet_id}"));
    }
    Ok((name, local_sheet_id))
}

fn convenience_defined_name_edits(
    object: &Map<String, Value>,
    sheets: &[SheetBinding],
) -> Result<Vec<Value>, String> {
    let mut output = Vec::new();
    for (field, native_name) in [
        ("printArea", "_xlnm.Print_Area"),
        ("printTitles", "_xlnm.Print_Titles"),
    ] {
        let Some(value) = object.get(field) else {
            continue;
        };
        match value {
            Value::Array(entries) => {
                for entry in entries {
                    let mut entry = entry
                        .as_object()
                        .cloned()
                        .ok_or_else(|| format!("definedNames.{field} entries must be objects"))?;
                    entry.insert("name".to_string(), Value::String(native_name.to_string()));
                    output.push(Value::Object(entry));
                }
            }
            Value::Object(entries) => {
                for (sheet_name, formula) in entries {
                    let sheet = sheets
                        .iter()
                        .find(|sheet| sheet.name == *sheet_name)
                        .ok_or_else(|| format!("unknown worksheet {sheet_name}"))?;
                    output.push(json!({
                        "name": native_name,
                        "localSheetId": sheet.local_sheet_id,
                        "formula": formula,
                    }));
                }
            }
            _ => {
                return Err(format!(
                    "definedNames.{field} must be an array or sheet/formula object"
                ));
            }
        }
    }
    Ok(output)
}

fn patch_defined_names(
    workbook_xml: &str,
    patch: &Value,
    sheets: &[SheetBinding],
) -> Result<String, String> {
    if patch.is_null() {
        return Ok(workbook_xml.to_string());
    }
    let object = patch
        .as_object()
        .ok_or_else(|| "definedNames must be an object".to_string())?;
    let mut upserts = object
        .get("upsert")
        .map(|value| {
            value
                .as_array()
                .cloned()
                .ok_or_else(|| "definedNames.upsert must be an array".to_string())
        })
        .transpose()?
        .unwrap_or_default();
    upserts.extend(convenience_defined_name_edits(object, sheets)?);
    let mut deletes: HashSet<(String, usize)> = object
        .get("delete")
        .map(|value| {
            value
                .as_array()
                .ok_or_else(|| "definedNames.delete must be an array".to_string())?
                .iter()
                .map(|entry| defined_name_key(entry, sheets))
                .collect::<Result<_, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    for value in &upserts {
        if delete_requested(value)
            || value
                .get("formula")
                .is_some_and(|formula| formula.is_null())
        {
            deletes.insert(defined_name_key(value, sheets)?);
        }
    }
    let mut output = workbook_xml.to_string();
    for (name, local_sheet_id) in deletes {
        let document = Document::parse(&output).map_err(|error| error.to_string())?;
        let root = document.root_element();
        let Some(container) = direct_child(root, "definedNames") else {
            continue;
        };
        let remove = direct_children(container, "definedName")
            .find(|node| {
                node.attribute("name") == Some(name.as_str())
                    && node
                        .attribute("localSheetId")
                        .and_then(|value| value.parse::<usize>().ok())
                        == Some(local_sheet_id)
            })
            .map(|node| node.range());
        if let Some(range) = remove {
            output = remove_range(&output, range);
        }
    }
    for value in upserts {
        if delete_requested(&value)
            || value
                .get("formula")
                .is_some_and(|formula| formula.is_null())
        {
            continue;
        }
        let object = value
            .as_object()
            .ok_or_else(|| "defined-name edit must be an object".to_string())?;
        let (name, local_sheet_id) = defined_name_key(&value, sheets)?;
        let formula = object
            .get("formula")
            .and_then(Value::as_str)
            .ok_or_else(|| "defined-name upsert requires formula string".to_string())?;
        let document = Document::parse(&output).map_err(|error| error.to_string())?;
        let root = document.root_element();
        let existing = direct_child(root, "definedNames").and_then(|container| {
            direct_children(container, "definedName").find(|node| {
                node.attribute("name") == Some(name.as_str())
                    && node
                        .attribute("localSheetId")
                        .and_then(|value| value.parse::<usize>().ok())
                        == Some(local_sheet_id)
            })
        });
        let reserved = ["name", "localSheetId", "sheetId", "sheet", "formula"];
        let mut changes = attribute_changes(object, &reserved)?;
        changes.retain(|(attribute, _)| attribute != "name" && attribute != "localSheetId");
        changes.push(("name".to_string(), Some(name.clone())));
        changes.push(("localSheetId".to_string(), Some(local_sheet_id.to_string())));
        if let Some(node) = existing {
            output = patch_open_tag(&output, node.range().start, &changes)?;
            let document = Document::parse(&output).map_err(|error| error.to_string())?;
            let root = document.root_element();
            let node = direct_child(root, "definedNames")
                .and_then(|container| {
                    direct_children(container, "definedName").find(|node| {
                        node.attribute("name") == Some(name.as_str())
                            && node.attribute("localSheetId")
                                == Some(local_sheet_id.to_string().as_str())
                    })
                })
                .ok_or_else(|| "defined name disappeared while patching".to_string())?;
            let range = element_text_range(&output, node)?;
            let escaped = xml_escape_text(formula);
            if output[range.clone()] != escaped {
                output.replace_range(range, &escaped);
            }
        } else {
            let prefix = qname_prefix(open_tag_qname(&output, root.range().start)?);
            let container_qname = qualify(prefix, "definedNames");
            let item_qname = qualify(prefix, "definedName");
            let item = format!(
                "<{item_qname}{}>{}</{item_qname}>",
                attrs_fragment(&changes),
                xml_escape_text(formula)
            );
            if let Some(container) = direct_child(root, "definedNames") {
                output = insert_child_before_close(&output, container, &item)?;
            } else {
                let container = format!("<{container_qname}>{item}</{container_qname}>");
                output = insert_ordered_child(
                    &output,
                    root,
                    "definedNames",
                    &container,
                    WORKBOOK_ORDER,
                )?;
            }
        }
    }
    let document = Document::parse(&output).map_err(|error| error.to_string())?;
    if let Some(container) = direct_child(document.root_element(), "definedNames") {
        let has_unknown_payload = container.attributes().len() > 0
            || container.children().any(|node| {
                (node.is_element() && local_name(node) != "definedName")
                    || node.is_comment()
                    || node.is_pi()
                    || node.text().is_some_and(|text| !text.trim().is_empty())
            });
        if direct_children(container, "definedName").next().is_none() && !has_unknown_payload {
            output = remove_range(&output, container.range());
        }
    }
    Ok(output)
}

fn next_relationship_id(relationships: &[Relationship]) -> String {
    let used: HashSet<&str> = relationships
        .iter()
        .map(|relationship| relationship.id.as_str())
        .collect();
    let mut index = 1u64;
    loop {
        let candidate = format!("rId{index}");
        if !used.contains(candidate.as_str()) {
            return candidate;
        }
        index += 1;
    }
}

fn ensure_relationship(
    parts: &mut BTreeMap<String, Vec<u8>>,
    source_part: &str,
    rel_type: &str,
    target_part: &str,
) -> Result<String, String> {
    let relationships = parse_relationships(parts, source_part)?;
    if let Some(existing) = relationships.iter().find(|relationship| {
        relationship.rel_type == rel_type
            && relationship
                .target_mode
                .as_deref()
                .is_none_or(|mode| !mode.eq_ignore_ascii_case("External"))
            && resolve_target(source_part, &relationship.target) == normalize_part(target_part)
    }) {
        return Ok(existing.id.clone());
    }
    let id = next_relationship_id(&relationships);
    let path = rels_part(source_part);
    let target = relative_target(source_part, target_part);
    let output = if let Some(bytes) = parts.get(&path) {
        let xml = std::str::from_utf8(bytes).map_err(|_| format!("{path} is not UTF-8 XML"))?;
        let document = Document::parse(xml).map_err(|error| format!("{path}: {error}"))?;
        let root = document.root_element();
        if local_name(root) != "Relationships"
            || root.tag_name().namespace() != Some(PACKAGE_REL_NS)
        {
            return Err(format!("{path} is not an OPC relationships part"));
        }
        let prefix = qname_prefix(open_tag_qname(xml, root.range().start)?);
        let qname = qualify(prefix, "Relationship");
        let relationship = format!(
            "<{qname} Id=\"{}\" Type=\"{}\" Target=\"{}\"/>",
            xml_escape_attribute(&id),
            xml_escape_attribute(rel_type),
            xml_escape_attribute(&target)
        );
        insert_child_before_close(xml, root, &relationship)?
    } else {
        let relationship = format!(
            "<Relationship Id=\"{}\" Type=\"{}\" Target=\"{}\"/>",
            xml_escape_attribute(&id),
            xml_escape_attribute(rel_type),
            xml_escape_attribute(&target)
        );
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Relationships xmlns=\"{PACKAGE_REL_NS}\">{relationship}</Relationships>"
        )
    };
    parts.insert(path, output.into_bytes());
    Ok(id)
}

fn ensure_content_type_override(
    parts: &mut BTreeMap<String, Vec<u8>>,
    part: &str,
    content_type: &str,
) -> Result<(), String> {
    let path = "[Content_Types].xml";
    let xml = part_text(parts, path)?.to_string();
    let document = Document::parse(&xml).map_err(|error| format!("{path}: {error}"))?;
    let root = document.root_element();
    let part_name = format!("/{}", normalize_part(part));
    if let Some(node) = direct_children(root, "Override")
        .find(|node| node.attribute("PartName") == Some(part_name.as_str()))
    {
        if node.attribute("ContentType") == Some(content_type) {
            return Ok(());
        }
        let updated = patch_open_tag(
            &xml,
            node.range().start,
            &[("ContentType".to_string(), Some(content_type.to_string()))],
        )?;
        parts.insert(path.to_string(), updated.into_bytes());
        return Ok(());
    }
    let prefix = qname_prefix(open_tag_qname(&xml, root.range().start)?);
    let qname = qualify(prefix, "Override");
    let child = format!(
        "<{qname} PartName=\"{}\" ContentType=\"{}\"/>",
        xml_escape_attribute(&part_name),
        xml_escape_attribute(content_type)
    );
    let updated = insert_child_before_close(&xml, root, &child)?;
    parts.insert(path.to_string(), updated.into_bytes());
    Ok(())
}

fn ensure_content_type_default(
    parts: &mut BTreeMap<String, Vec<u8>>,
    extension: &str,
    content_type: &str,
) -> Result<(), String> {
    let path = "[Content_Types].xml";
    let xml = part_text(parts, path)?.to_string();
    let document = Document::parse(&xml).map_err(|error| format!("{path}: {error}"))?;
    let root = document.root_element();
    if let Some(node) =
        direct_children(root, "Default").find(|node| node.attribute("Extension") == Some(extension))
    {
        if node.attribute("ContentType") == Some(content_type) {
            return Ok(());
        }
        let updated = patch_open_tag(
            &xml,
            node.range().start,
            &[("ContentType".to_string(), Some(content_type.to_string()))],
        )?;
        parts.insert(path.to_string(), updated.into_bytes());
        return Ok(());
    }
    let prefix = qname_prefix(open_tag_qname(&xml, root.range().start)?);
    let qname = qualify(prefix, "Default");
    let child = format!(
        "<{qname} Extension=\"{}\" ContentType=\"{}\"/>",
        xml_escape_attribute(extension),
        xml_escape_attribute(content_type)
    );
    let updated = insert_child_before_close(&xml, root, &child)?;
    parts.insert(path.to_string(), updated.into_bytes());
    Ok(())
}

fn next_numbered_part(parts: &BTreeMap<String, Vec<u8>>, prefix: &str, suffix: &str) -> String {
    let mut index = 1u64;
    loop {
        let candidate = format!("{prefix}{index}{suffix}");
        if !parts.contains_key(&candidate) {
            return candidate;
        }
        index += 1;
    }
}

fn ensure_namespace_prefix(xml: &str, prefix: &str, namespace: &str) -> Result<String, String> {
    let document = Document::parse(xml).map_err(|error| error.to_string())?;
    let root = document.root_element();
    if root.lookup_namespace_uri(Some(prefix)) == Some(namespace) {
        return Ok(xml.to_string());
    }
    patch_open_tag(
        xml,
        root.range().start,
        &[(format!("xmlns:{prefix}"), Some(namespace.to_string()))],
    )
}

fn ensure_legacy_drawing(
    parts: &mut BTreeMap<String, Vec<u8>>,
    sheet_part: &str,
) -> Result<String, String> {
    let original_sheet_xml = part_text(parts, sheet_part)?.to_string();
    let sheet_document =
        Document::parse(&original_sheet_xml).map_err(|error| format!("{sheet_part}: {error}"))?;
    let legacy_id = direct_child(sheet_document.root_element(), "legacyDrawing")
        .and_then(|node| {
            node.attribute((REL_NS, "id"))
                .or_else(|| node.attribute("r:id"))
        })
        .map(str::to_string);
    let header_footer_id = direct_child(sheet_document.root_element(), "legacyDrawingHF")
        .and_then(|node| {
            node.attribute((REL_NS, "id"))
                .or_else(|| node.attribute("r:id"))
        })
        .map(str::to_string);
    let relationships = parse_relationships(parts, sheet_part)?;
    let selected = legacy_id
        .as_deref()
        .and_then(|id| {
            relationships
                .iter()
                .find(|relationship| relationship.id == id && relationship.rel_type == VML_REL)
        })
        .or_else(|| {
            relationships.iter().find(|relationship| {
                relationship.rel_type == VML_REL
                    && header_footer_id.as_deref() != Some(relationship.id.as_str())
            })
        })
        .map(|relationship| {
            (
                relationship.id.clone(),
                resolve_target(sheet_part, &relationship.target),
            )
        });
    let (relationship_id, vml_part) = if let Some((id, part)) = selected {
        (id, part)
    } else {
        let vml_part = next_numbered_part(parts, "xl/drawings/vmlDrawing", ".vml");
        let vml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<xml xmlns:v="urn:schemas-microsoft-com:vml" xmlns:o="urn:schemas-microsoft-com:office:office" xmlns:x="urn:schemas-microsoft-com:office:excel"><o:shapelayout v:ext="edit"><o:idmap v:ext="edit" data="1"/></o:shapelayout><v:shapetype id="_x0000_t202" coordsize="21600,21600" o:spt="202" path="m,l,21600r21600,l21600,xe"><v:stroke joinstyle="miter"/><v:path gradientshapeok="t" o:connecttype="rect"/></v:shapetype></xml>"#;
        parts.insert(vml_part.clone(), vml.as_bytes().to_vec());
        ensure_content_type_default(parts, "vml", VML_CONTENT_TYPE)?;
        let relationship_id = ensure_relationship(parts, sheet_part, VML_REL, &vml_part)?;
        (relationship_id, vml_part)
    };
    ensure_content_type_default(parts, "vml", VML_CONTENT_TYPE)?;
    let mut sheet_xml = ensure_namespace_prefix(&original_sheet_xml, "r", REL_NS)?;
    let document = Document::parse(&sheet_xml).map_err(|error| format!("{sheet_part}: {error}"))?;
    let root = document.root_element();
    if let Some(legacy_drawing) = direct_child(root, "legacyDrawing") {
        let current_id = legacy_drawing
            .attribute((REL_NS, "id"))
            .or_else(|| legacy_drawing.attribute("r:id"));
        if current_id != Some(relationship_id.as_str()) {
            sheet_xml = patch_open_tag(
                &sheet_xml,
                legacy_drawing.range().start,
                &[("r:id".to_string(), Some(relationship_id.clone()))],
            )?;
        }
    } else {
        let prefix = qname_prefix(open_tag_qname(&sheet_xml, root.range().start)?);
        let qname = qualify(prefix, "legacyDrawing");
        sheet_xml = insert_ordered_child(
            &sheet_xml,
            root,
            "legacyDrawing",
            &format!(
                "<{qname} r:id=\"{}\"/>",
                xml_escape_attribute(&relationship_id)
            ),
            WORKSHEET_ORDER,
        )?;
    }
    parts.insert(sheet_part.to_string(), sheet_xml.into_bytes());
    Ok(vml_part)
}

fn ensure_comments_part(
    parts: &mut BTreeMap<String, Vec<u8>>,
    sheet_part: &str,
) -> Result<String, String> {
    if let Some((_id, comments_part)) = relationship_target(parts, sheet_part, COMMENTS_REL)? {
        ensure_content_type_override(parts, &comments_part, COMMENTS_CONTENT_TYPE)?;
        ensure_legacy_drawing(parts, sheet_part)?;
        return Ok(comments_part);
    }
    let comments_part = next_numbered_part(parts, "xl/comments", ".xml");
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><comments xmlns=\"{MAIN_NS}\"><authors/><commentList/></comments>"
    );
    parts.insert(comments_part.clone(), xml.into_bytes());
    ensure_content_type_override(parts, &comments_part, COMMENTS_CONTENT_TYPE)?;
    ensure_relationship(parts, sheet_part, COMMENTS_REL, &comments_part)?;
    ensure_legacy_drawing(parts, sheet_part)?;
    Ok(comments_part)
}

fn ensure_persons_part(
    parts: &mut BTreeMap<String, Vec<u8>>,
    workbook_part: &str,
) -> Result<String, String> {
    if let Some((_id, persons_part)) = relationship_target(parts, workbook_part, PERSON_REL)? {
        ensure_content_type_override(parts, &persons_part, PERSON_CONTENT_TYPE)?;
        return Ok(persons_part);
    }
    let persons_part = if !parts.contains_key("xl/persons/person.xml") {
        "xl/persons/person.xml".to_string()
    } else {
        next_numbered_part(parts, "xl/persons/person", ".xml")
    };
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><personList xmlns=\"{THREADED_NS}\"></personList>"
    );
    parts.insert(persons_part.clone(), xml.into_bytes());
    ensure_content_type_override(parts, &persons_part, PERSON_CONTENT_TYPE)?;
    ensure_relationship(parts, workbook_part, PERSON_REL, &persons_part)?;
    Ok(persons_part)
}

fn ensure_threaded_part(
    parts: &mut BTreeMap<String, Vec<u8>>,
    workbook_part: &str,
    sheet_part: &str,
) -> Result<String, String> {
    if let Some((_id, threaded_part)) =
        relationship_target(parts, sheet_part, THREADED_COMMENTS_REL)?
    {
        ensure_content_type_override(parts, &threaded_part, THREADED_COMMENTS_CONTENT_TYPE)?;
        ensure_persons_part(parts, workbook_part)?;
        return Ok(threaded_part);
    }
    let threaded_part = next_numbered_part(parts, "xl/threadedComments/threadedComment", ".xml");
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><threadedComments xmlns=\"{THREADED_NS}\"></threadedComments>"
    );
    parts.insert(threaded_part.clone(), xml.into_bytes());
    ensure_content_type_override(parts, &threaded_part, THREADED_COMMENTS_CONTENT_TYPE)?;
    ensure_relationship(parts, sheet_part, THREADED_COMMENTS_REL, &threaded_part)?;
    ensure_persons_part(parts, workbook_part)?;
    Ok(threaded_part)
}

fn string_list(value: Option<&Value>, field: &str) -> Result<Vec<String>, String> {
    value
        .map(|value| {
            value
                .as_array()
                .ok_or_else(|| format!("{field} must be an array"))?
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_string)
                        .ok_or_else(|| format!("{field} entries must be strings"))
                })
                .collect()
        })
        .transpose()
        .map(Option::unwrap_or_default)
}

fn cell_row_column(reference: &str) -> Result<(u32, u32), String> {
    let reference = reference
        .rsplit_once('!')
        .map(|(_, reference)| reference)
        .unwrap_or(reference)
        .replace('$', "");
    let bytes = reference.as_bytes();
    let mut cursor = 0usize;
    let mut column = 0u32;
    while cursor < bytes.len() && bytes[cursor].is_ascii_alphabetic() {
        column = column
            .checked_mul(26)
            .and_then(|value| {
                value.checked_add((bytes[cursor].to_ascii_uppercase() - b'A' + 1) as u32)
            })
            .ok_or_else(|| format!("invalid cell reference {reference}"))?;
        cursor += 1;
    }
    if cursor == 0 || cursor == bytes.len() || !bytes[cursor..].iter().all(u8::is_ascii_digit) {
        return Err(format!("invalid cell reference {reference}"));
    }
    let row: u32 = reference[cursor..]
        .parse()
        .map_err(|_| format!("invalid cell reference {reference}"))?;
    if row == 0 || column == 0 {
        return Err(format!("invalid cell reference {reference}"));
    }
    Ok((row - 1, column - 1))
}

fn ensure_comment_author(xml: &str, author: &str) -> Result<(String, usize), String> {
    let document = Document::parse(xml).map_err(|error| error.to_string())?;
    let root = document.root_element();
    if let Some(authors) = direct_child(root, "authors") {
        let existing: Vec<Node<'_, '_>> = direct_children(authors, "author").collect();
        if let Some(index) = existing
            .iter()
            .position(|node| node.text().unwrap_or_default() == author)
        {
            return Ok((xml.to_string(), index));
        }
        let prefix = qname_prefix(open_tag_qname(xml, authors.range().start)?);
        let qname = qualify(prefix, "author");
        let updated = insert_child_before_close(
            xml,
            authors,
            &format!("<{qname}>{}</{qname}>", xml_escape_text(author)),
        )?;
        return Ok((updated, existing.len()));
    }
    let prefix = qname_prefix(open_tag_qname(xml, root.range().start)?);
    let authors_qname = qualify(prefix, "authors");
    let author_qname = qualify(prefix, "author");
    let fragment = format!(
        "<{authors_qname}><{author_qname}>{}</{author_qname}></{authors_qname}>",
        xml_escape_text(author)
    );
    let updated = if let Some(comment_list) = direct_child(root, "commentList") {
        let mut output = xml.to_string();
        output.insert_str(comment_list.range().start, &fragment);
        output
    } else {
        insert_child_before_close(xml, root, &fragment)?
    };
    Ok((updated, 0))
}

fn validate_text_fragment(fragment: &str, expected: &str) -> Result<(), String> {
    let document =
        Document::parse(fragment).map_err(|error| format!("invalid {expected} XML: {error}"))?;
    if local_name(document.root_element()) != expected {
        return Err(format!("XML fragment root must be {expected}"));
    }
    Ok(())
}

fn replace_element_text_preserving_runs(
    xml: &str,
    element: Node<'_, '_>,
    text: &str,
) -> Result<String, String> {
    let text_nodes: Vec<Node<'_, '_>> = element
        .descendants()
        .filter(|node| node.is_element() && local_name(*node) == "t")
        .collect();
    if text_nodes.is_empty() {
        let prefix = qname_prefix(open_tag_qname(xml, element.range().start)?);
        let qname = qualify(prefix, "t");
        return insert_child_before_close(
            xml,
            element,
            &format!(
                "<{qname} xml:space=\"preserve\">{}</{qname}>",
                xml_escape_text(text)
            ),
        );
    }
    // The page-review UI intentionally exposes a plain-text fallback for legacy notes.  A note
    // can nevertheless contain several rich-text runs.  Putting the complete replacement into
    // the first <t> (and emptying every later one) silently applies the first run's formatting to
    // the whole note on the next Excel open.  Keep the existing run boundaries instead: every
    // non-final text node retains its original Unicode-scalar length and the final node receives
    // the remainder.  This deterministic fallback cannot infer an unavailable character
    // selection, but—most importantly—leaves every rPr/unknown child byte-for-byte untouched.
    let original_lengths: Vec<usize> = text_nodes
        .iter()
        .map(|node| node.text().unwrap_or_default().chars().count())
        .collect();
    let replacement_chars: Vec<char> = text.chars().collect();
    let mut replacement_offset = 0usize;
    let mut replacement_chunks = Vec::with_capacity(text_nodes.len());
    for (index, original_length) in original_lengths.iter().enumerate() {
        let end = if index + 1 == original_lengths.len() {
            replacement_chars.len()
        } else {
            replacement_offset
                .saturating_add(*original_length)
                .min(replacement_chars.len())
        };
        replacement_chunks.push(
            replacement_chars[replacement_offset.min(replacement_chars.len())..end]
                .iter()
                .collect::<String>(),
        );
        replacement_offset = end;
    }
    let mut replacements = Vec::new();
    for (index, node) in text_nodes.iter().enumerate() {
        let open_end = scan_open_tag_end(xml, node.range().start)?;
        if xml[node.range().start..open_end].trim_end().ends_with("/>") {
            let qname = open_tag_qname(xml, node.range().start)?;
            let mut open = xml[node.range().start..open_end].to_string();
            let slash = open.rfind("/>").unwrap();
            open.replace_range(slash..slash + 2, ">");
            let content = xml_escape_text(&replacement_chunks[index]);
            replacements.push((node.range(), format!("{open}{content}</{qname}>")));
        } else {
            let range = element_text_range(xml, *node)?;
            replacements.push((range, xml_escape_text(&replacement_chunks[index])));
        }
    }
    replacements.sort_by(|left, right| right.0.start.cmp(&left.0.start));
    let mut output = xml.to_string();
    for (range, replacement) in replacements {
        output.replace_range(range, &replacement);
    }
    Ok(output)
}

fn vml_note_shape_range(
    vml_xml: &str,
    row: u32,
    column: u32,
) -> Result<Option<Range<usize>>, String> {
    let document = Document::parse(vml_xml).map_err(|error| error.to_string())?;
    Ok(document
        .descendants()
        .filter(|node| node.is_element() && local_name(*node) == "shape")
        .find(|shape| {
            shape.descendants().any(|node| {
                node.is_element()
                    && local_name(node) == "ClientData"
                    && node.attribute("ObjectType") == Some("Note")
                    && direct_child(node, "Row")
                        .and_then(|node| node.text())
                        .and_then(|value| value.parse::<u32>().ok())
                        == Some(row)
                    && direct_child(node, "Column")
                        .and_then(|node| node.text())
                        .and_then(|value| value.parse::<u32>().ok())
                        == Some(column)
            })
        })
        .map(|node| node.range()))
}

fn ensure_vml_note(vml_xml: &str, reference: &str) -> Result<String, String> {
    let (row, column) = cell_row_column(reference)?;
    if vml_note_shape_range(vml_xml, row, column)?.is_some() {
        return Ok(vml_xml.to_string());
    }
    let document = Document::parse(vml_xml).map_err(|error| error.to_string())?;
    let root = document.root_element();
    let max_id = document
        .descendants()
        .filter(|node| node.is_element() && local_name(*node) == "shape")
        .filter_map(|node| {
            node.attribute("id")?
                .strip_prefix("_x0000_s")?
                .parse::<u64>()
                .ok()
        })
        .max()
        .unwrap_or(1024);
    let shape_id = max_id + 1;
    let shape = format!(
        "<v:shape id=\"_x0000_s{shape_id}\" type=\"#_x0000_t202\" style=\"position:absolute;margin-left:59.25pt;margin-top:1.5pt;width:108pt;height:59.25pt;z-index:1;visibility:hidden\" fillcolor=\"#ffffe1\" o:insetmode=\"auto\"><v:fill color2=\"#ffffe1\"/><v:shadow on=\"t\" color=\"black\" obscured=\"t\"/><v:path o:connecttype=\"none\"/><v:textbox style=\"mso-direction-alt:auto\"><div style=\"text-align:left\"/></v:textbox><x:ClientData ObjectType=\"Note\"><x:MoveWithCells/><x:SizeWithCells/><x:Anchor>{column}, 15, {row}, 2, {}, 31, {}, 1</x:Anchor><x:AutoFill>False</x:AutoFill><x:Row>{row}</x:Row><x:Column>{column}</x:Column></x:ClientData></v:shape>",
        column + 2,
        row + 4
    );
    insert_child_before_close(vml_xml, root, &shape)
}

fn remove_vml_note(vml_xml: &str, reference: &str) -> Result<String, String> {
    let (row, column) = cell_row_column(reference)?;
    if let Some(range) = vml_note_shape_range(vml_xml, row, column)? {
        Ok(remove_range(vml_xml, range))
    } else {
        Ok(vml_xml.to_string())
    }
}

fn note_operations(patch: &Value) -> Result<(Vec<Value>, Vec<String>), String> {
    if let Some(items) = patch.as_array() {
        return Ok((items.clone(), Vec::new()));
    }
    let object = patch
        .as_object()
        .ok_or_else(|| "notes must be an object or array".to_string())?;
    let upsert = object
        .get("upsert")
        .or_else(|| object.get("items"))
        .map(|value| {
            value
                .as_array()
                .cloned()
                .ok_or_else(|| "notes.upsert must be an array".to_string())
        })
        .transpose()?
        .unwrap_or_default();
    let delete = string_list(object.get("delete"), "notes.delete")?;
    Ok((upsert, delete))
}

fn apply_notes_edit(
    parts: &mut BTreeMap<String, Vec<u8>>,
    sheet_part: &str,
    patch: &Value,
) -> Result<(), String> {
    let (upserts, mut deletes) = note_operations(patch)?;
    for value in &upserts {
        if delete_requested(value) {
            let reference = value
                .get("ref")
                .and_then(Value::as_str)
                .ok_or_else(|| "deleted note requires ref".to_string())?;
            deletes.push(reference.to_string());
        }
    }
    if upserts.is_empty() && deletes.is_empty() {
        return Ok(());
    }
    let existing_part = relationship_target(parts, sheet_part, COMMENTS_REL)?;
    if existing_part.is_none() && upserts.iter().all(|value| delete_requested(value)) {
        return Ok(());
    }
    let comments_part = if let Some((_id, part)) = existing_part {
        part
    } else {
        ensure_comments_part(parts, sheet_part)?
    };
    let vml_part = ensure_legacy_drawing(parts, sheet_part)?;
    let mut xml = part_text(parts, &comments_part)?.to_string();
    let mut vml = part_text(parts, &vml_part)?.to_string();
    let mut deleted = HashSet::new();
    for reference in deletes {
        if !deleted.insert(reference.clone()) {
            continue;
        }
        let document =
            Document::parse(&xml).map_err(|error| format!("{comments_part}: {error}"))?;
        if let Some(comment) = document.descendants().find(|node| {
            node.is_element()
                && local_name(*node) == "comment"
                && node.attribute("ref") == Some(reference.as_str())
        }) {
            xml = remove_range(&xml, comment.range());
        }
        vml = remove_vml_note(&vml, &reference)?;
    }
    for value in upserts {
        if delete_requested(&value) {
            continue;
        }
        let object = value
            .as_object()
            .ok_or_else(|| "note upsert must be an object".to_string())?;
        let reference = object
            .get("ref")
            .and_then(Value::as_str)
            .ok_or_else(|| "note upsert requires ref".to_string())?;
        cell_row_column(reference)?;
        let author = object.get("author").and_then(Value::as_str);
        let author_id = if let Some(author) = author {
            let result = ensure_comment_author(&xml, author)?;
            xml = result.0;
            Some(result.1)
        } else {
            None
        };
        let document =
            Document::parse(&xml).map_err(|error| format!("{comments_part}: {error}"))?;
        let existing = document.descendants().find(|node| {
            node.is_element()
                && local_name(*node) == "comment"
                && node.attribute("ref") == Some(reference)
        });
        if let Some(comment) = existing {
            let reserved = ["ref", "author", "text", "textXml"];
            let mut changes = attribute_changes(object, &reserved)?;
            if let Some(author_id) = author_id {
                changes.push(("authorId".to_string(), Some(author_id.to_string())));
            }
            xml = patch_open_tag(&xml, comment.range().start, &changes)?;
            if object.contains_key("text") || object.contains_key("textXml") {
                let document =
                    Document::parse(&xml).map_err(|error| format!("{comments_part}: {error}"))?;
                let comment = document
                    .descendants()
                    .find(|node| {
                        node.is_element()
                            && local_name(*node) == "comment"
                            && node.attribute("ref") == Some(reference)
                    })
                    .ok_or_else(|| "note disappeared while patching".to_string())?;
                if let Some(text_xml) = object.get("textXml").and_then(Value::as_str) {
                    validate_text_fragment(text_xml, "text")?;
                    if let Some(text) = direct_child(comment, "text") {
                        let mut output = xml.clone();
                        output.replace_range(text.range(), text_xml);
                        xml = output;
                    } else {
                        xml = insert_child_before_close(&xml, comment, text_xml)?;
                    }
                } else if let Some(text) = object.get("text").and_then(Value::as_str) {
                    if let Some(text_node) = direct_child(comment, "text") {
                        xml = replace_element_text_preserving_runs(&xml, text_node, text)?;
                    } else {
                        let prefix = qname_prefix(open_tag_qname(&xml, comment.range().start)?);
                        let text_qname = qualify(prefix, "text");
                        let t_qname = qualify(prefix, "t");
                        xml = insert_child_before_close(
                            &xml,
                            comment,
                            &format!(
                                "<{text_qname}><{t_qname} xml:space=\"preserve\">{}</{t_qname}></{text_qname}>",
                                xml_escape_text(text)
                            ),
                        )?;
                    }
                }
            }
        } else {
            let author_id = if let Some(author_id) = author_id {
                author_id
            } else {
                let result = ensure_comment_author(&xml, "UniCell")?;
                xml = result.0;
                result.1
            };
            let text_fragment = if let Some(fragment) =
                object.get("textXml").and_then(Value::as_str)
            {
                validate_text_fragment(fragment, "text")?;
                fragment.to_string()
            } else {
                let text = object
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let document = Document::parse(&xml).map_err(|error| error.to_string())?;
                let prefix =
                    qname_prefix(open_tag_qname(&xml, document.root_element().range().start)?);
                let text_qname = qualify(prefix, "text");
                let t_qname = qualify(prefix, "t");
                format!(
                    "<{text_qname}><{t_qname} xml:space=\"preserve\">{}</{t_qname}></{text_qname}>",
                    xml_escape_text(text)
                )
            };
            let document = Document::parse(&xml).map_err(|error| error.to_string())?;
            let root = document.root_element();
            let prefix = qname_prefix(open_tag_qname(&xml, root.range().start)?);
            let comment_qname = qualify(prefix, "comment");
            let comment_list_qname = qualify(prefix, "commentList");
            let mut attributes = attribute_changes(object, &["ref", "author", "text", "textXml"])?;
            attributes.retain(|(name, _)| name != "ref" && name != "authorId");
            attributes.push(("ref".to_string(), Some(reference.to_string())));
            attributes.push(("authorId".to_string(), Some(author_id.to_string())));
            let fragment = format!(
                "<{comment_qname}{}>{text_fragment}</{comment_qname}>",
                attrs_fragment(&attributes)
            );
            if let Some(list) = direct_child(root, "commentList") {
                xml = insert_child_before_close(&xml, list, &fragment)?;
            } else {
                xml = insert_child_before_close(
                    &xml,
                    root,
                    &format!("<{comment_list_qname}>{fragment}</{comment_list_qname}>"),
                )?;
            }
        }
        vml = ensure_vml_note(&vml, reference)?;
    }
    parts.insert(comments_part, xml.into_bytes());
    parts.insert(vml_part, vml.into_bytes());
    Ok(())
}

fn fnv1a64(seed: &str, offset: u64) -> u64 {
    let mut hash = 0xcbf29ce484222325u64 ^ offset;
    for byte in seed.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn stable_guid(seed: &str) -> String {
    let left = fnv1a64(seed, 0);
    let right = fnv1a64(seed, 0x9e3779b97f4a7c15);
    let bytes = [left.to_be_bytes(), right.to_be_bytes()].concat();
    format!(
        "{{{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

fn unique_guid(seed: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let tick = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    stable_guid(&format!("{seed}:{nanos}:{tick}"))
}

fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += if month <= 2 { 1 } else { 0 };
    (year, month as u32, day as u32)
}

fn current_rfc3339() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    let days = seconds.div_euclid(86_400);
    let seconds_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn ensure_person(
    parts: &mut BTreeMap<String, Vec<u8>>,
    workbook_part: &str,
    requested_id: Option<&str>,
    display_name: &str,
    user_id: Option<&str>,
    provider_id: Option<&str>,
) -> Result<String, String> {
    let persons_part = ensure_persons_part(parts, workbook_part)?;
    let mut xml = part_text(parts, &persons_part)?.to_string();
    let document = Document::parse(&xml).map_err(|error| format!("{persons_part}: {error}"))?;
    if let Some(person) = document.descendants().find(|node| {
        node.is_element()
            && local_name(*node) == "person"
            && (requested_id.is_some_and(|id| node.attribute("id") == Some(id))
                || (requested_id.is_none() && node.attribute("displayName") == Some(display_name)))
    }) {
        let id = person
            .attribute("id")
            .ok_or_else(|| "person is missing id".to_string())?
            .to_string();
        let mut changes = vec![("displayName".to_string(), Some(display_name.to_string()))];
        if let Some(user_id) = user_id {
            changes.push(("userId".to_string(), Some(user_id.to_string())));
        }
        if let Some(provider_id) = provider_id {
            changes.push(("providerId".to_string(), Some(provider_id.to_string())));
        }
        xml = patch_open_tag(&xml, person.range().start, &changes)?;
        parts.insert(persons_part, xml.into_bytes());
        return Ok(id);
    }
    let id = requested_id
        .map(str::to_string)
        .unwrap_or_else(|| unique_guid(&format!("person:{display_name}:{}", xml.len())));
    let root = document.root_element();
    let prefix = qname_prefix(open_tag_qname(&xml, root.range().start)?);
    let qname = qualify(prefix, "person");
    let user_id = user_id.unwrap_or(display_name);
    let provider_id = provider_id.unwrap_or("None");
    let fragment = format!(
        "<{qname} displayName=\"{}\" id=\"{}\" userId=\"{}\" providerId=\"{}\"/>",
        xml_escape_attribute(display_name),
        xml_escape_attribute(&id),
        xml_escape_attribute(user_id),
        xml_escape_attribute(provider_id)
    );
    xml = insert_child_before_close(&xml, root, &fragment)?;
    parts.insert(persons_part, xml.into_bytes());
    Ok(id)
}

fn threaded_operations(
    patch: &Value,
) -> Result<(Vec<Value>, HashSet<String>, HashSet<String>), String> {
    if let Some(items) = patch.as_array() {
        return Ok((items.clone(), HashSet::new(), HashSet::new()));
    }
    let object = patch
        .as_object()
        .ok_or_else(|| "threadedComments must be an object or array".to_string())?;
    let upsert = object
        .get("upsert")
        .or_else(|| object.get("items"))
        .map(|value| {
            value
                .as_array()
                .cloned()
                .ok_or_else(|| "threadedComments.upsert must be an array".to_string())
        })
        .transpose()?
        .unwrap_or_default();
    let delete_ids = string_list(object.get("deleteIds"), "threadedComments.deleteIds")?
        .into_iter()
        .collect();
    let delete_refs = string_list(object.get("deleteRefs"), "threadedComments.deleteRefs")?
        .into_iter()
        .collect();
    Ok((upsert, delete_ids, delete_refs))
}

fn mention_key(value: &Value) -> Option<String> {
    value
        .get("mentionId")
        .or_else(|| {
            value
                .get("attributes")
                .and_then(|attributes| attributes.get("mentionId"))
        })
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            let person = value
                .get("personId")
                .or_else(|| {
                    value
                        .get("attributes")
                        .and_then(|attributes| attributes.get("personId"))
                })?
                .as_str()?;
            let start = value.get("startIndex").or_else(|| {
                value
                    .get("attributes")
                    .and_then(|attributes| attributes.get("startIndex"))
            })?;
            Some(format!("{person}:{start}"))
        })
}

fn mention_fragment(prefix: &str, value: &Value) -> Result<String, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "mention must be an object".to_string())?;
    let attributes = attribute_changes(object, &[])?;
    for required in ["personId", "startIndex", "length"] {
        if !attributes
            .iter()
            .any(|(name, value)| name == required && value.is_some())
        {
            return Err(format!("mention requires {required}"));
        }
    }
    let qname = qualify(prefix, "mention");
    Ok(format!("<{qname}{}/>", attrs_fragment(&attributes)))
}

fn patch_threaded_mentions(
    xml: &str,
    comment: Node<'_, '_>,
    patch: &Value,
) -> Result<String, String> {
    let existing = direct_child(comment, "mentions");
    if patch.is_null() {
        return Ok(existing
            .map(|node| remove_range(xml, node.range()))
            .unwrap_or_else(|| xml.to_string()));
    }
    let requested = patch
        .as_array()
        .ok_or_else(|| "threaded comment mentions must be an array or null".to_string())?;
    let mut desired = BTreeMap::new();
    for value in requested {
        let key = mention_key(value).ok_or_else(|| {
            "mention requires mentionId or the personId/startIndex pair".to_string()
        })?;
        if desired.insert(key.clone(), value.clone()).is_some() {
            return Err(format!("duplicate mention {key}"));
        }
    }
    let prefix = qname_prefix(open_tag_qname(xml, comment.range().start)?).to_string();
    let mut output = xml.to_string();
    if let Some(container) = existing {
        let mut removals = Vec::new();
        let mut existing_keys = HashSet::new();
        for mention in direct_children(container, "mention") {
            let value = attrs_json(mention);
            let Some(key) = mention_key(&value) else {
                continue;
            };
            existing_keys.insert(key.clone());
            if !desired.contains_key(&key) {
                removals.push(mention.range());
            }
        }
        removals.sort_by(|left, right| right.start.cmp(&left.start));
        for range in removals {
            output = remove_range(&output, range);
        }
        for (key, value) in desired {
            let document = Document::parse(&output).map_err(|error| error.to_string())?;
            let comment = document
                .descendants()
                .find(|node| {
                    node.is_element()
                        && local_name(*node) == "threadedComment"
                        && node.attribute("id") == comment.attribute("id")
                })
                .ok_or_else(|| "threaded comment disappeared while editing mentions".to_string())?;
            let container = direct_child(comment, "mentions")
                .ok_or_else(|| "mentions disappeared while patching".to_string())?;
            if existing_keys.contains(&key) {
                let range = direct_children(container, "mention")
                    .find(|node| mention_key(&attrs_json(*node)).as_deref() == Some(key.as_str()))
                    .map(|node| node.range());
                if let Some(range) = range {
                    let object = value.as_object().unwrap();
                    output =
                        patch_open_tag(&output, range.start, &attribute_changes(object, &[])?)?;
                }
            } else {
                output = insert_child_before_close(
                    &output,
                    container,
                    &mention_fragment(&prefix, &value)?,
                )?;
            }
        }
        let document = Document::parse(&output).map_err(|error| error.to_string())?;
        let comment = document
            .descendants()
            .find(|node| {
                node.is_element()
                    && local_name(*node) == "threadedComment"
                    && node.attribute("id") == comment.attribute("id")
            })
            .ok_or_else(|| "threaded comment disappeared while editing mentions".to_string())?;
        let container = direct_child(comment, "mentions").unwrap();
        if direct_children(container, "mention").next().is_none() {
            let has_unknown = container.children().any(|node| {
                (node.is_element() && local_name(node) != "mention")
                    || node.is_comment()
                    || node.is_pi()
                    || node.text().is_some_and(|text| !text.trim().is_empty())
            });
            if !has_unknown {
                output = remove_range(&output, container.range());
            }
        }
    } else if !desired.is_empty() {
        let mentions_qname = qualify(&prefix, "mentions");
        let mut children = String::new();
        for value in desired.values() {
            children.push_str(&mention_fragment(&prefix, value)?);
        }
        output = insert_child_before_close(
            xml,
            comment,
            &format!("<{mentions_qname}>{children}</{mentions_qname}>"),
        )?;
    }
    Ok(output)
}

fn apply_threaded_comments_edit(
    parts: &mut BTreeMap<String, Vec<u8>>,
    workbook_part: &str,
    sheet_part: &str,
    patch: &Value,
) -> Result<(), String> {
    let (upserts, mut delete_ids, mut delete_refs) = threaded_operations(patch)?;
    for value in &upserts {
        if delete_requested(value) {
            if let Some(id) = value.get("id").and_then(Value::as_str) {
                delete_ids.insert(id.to_string());
            } else if let Some(reference) = value.get("ref").and_then(Value::as_str) {
                delete_refs.insert(reference.to_string());
            } else {
                return Err("deleted threaded comment requires id or ref".to_string());
            }
        }
    }
    if upserts.is_empty() && delete_ids.is_empty() && delete_refs.is_empty() {
        return Ok(());
    }
    let existing = relationship_target(parts, sheet_part, THREADED_COMMENTS_REL)?;
    if existing.is_none() && upserts.iter().all(delete_requested) {
        return Ok(());
    }
    let threaded_part = if let Some((_id, part)) = existing {
        part
    } else {
        ensure_threaded_part(parts, workbook_part, sheet_part)?
    };
    let mut xml = part_text(parts, &threaded_part)?.to_string();
    if !delete_ids.is_empty() || !delete_refs.is_empty() {
        let document =
            Document::parse(&xml).map_err(|error| format!("{threaded_part}: {error}"))?;
        let mut ranges: Vec<Range<usize>> = document
            .descendants()
            .filter(|node| {
                node.is_element()
                    && local_name(*node) == "threadedComment"
                    && (node
                        .attribute("id")
                        .is_some_and(|id| delete_ids.contains(id))
                        || node
                            .attribute("ref")
                            .is_some_and(|reference| delete_refs.contains(reference)))
            })
            .map(|node| node.range())
            .collect();
        ranges.sort_by(|left, right| right.start.cmp(&left.start));
        for range in ranges {
            xml = remove_range(&xml, range);
        }
    }
    for value in upserts {
        if delete_requested(&value) {
            continue;
        }
        let object = value
            .as_object()
            .ok_or_else(|| "threaded comment upsert must be an object".to_string())?;
        let reference = object
            .get("ref")
            .and_then(Value::as_str)
            .ok_or_else(|| "threaded comment upsert requires ref".to_string())?;
        cell_row_column(reference)?;
        let requested_id = object.get("id").and_then(Value::as_str);
        let document =
            Document::parse(&xml).map_err(|error| format!("{threaded_part}: {error}"))?;
        let existing = document.descendants().find(|node| {
            node.is_element()
                && local_name(*node) == "threadedComment"
                && requested_id
                    .map(|id| node.attribute("id") == Some(id))
                    .unwrap_or_else(|| {
                        node.attribute("ref") == Some(reference)
                            && node.attribute("parentId").is_none()
                    })
        });

        let person_object = object.get("person").and_then(Value::as_object);
        let direct_person_id = object.get("personId").and_then(Value::as_str).or_else(|| {
            person_object
                .and_then(|person| person.get("id"))
                .and_then(Value::as_str)
        });
        let display_name = object
            .get("author")
            .or_else(|| object.get("displayName"))
            .and_then(Value::as_str)
            .or_else(|| {
                person_object
                    .and_then(|person| person.get("displayName"))
                    .and_then(Value::as_str)
            });
        let person_id = if display_name.is_some() || direct_person_id.is_none() {
            ensure_person(
                parts,
                workbook_part,
                direct_person_id,
                display_name.unwrap_or("UniCell"),
                person_object
                    .and_then(|person| person.get("userId"))
                    .and_then(Value::as_str),
                person_object
                    .and_then(|person| person.get("providerId"))
                    .and_then(Value::as_str),
            )?
        } else {
            let requested = direct_person_id.unwrap();
            let (_part, persons, _values) = inspect_persons(parts, workbook_part)?;
            if persons.contains_key(requested) {
                requested.to_string()
            } else {
                ensure_person(parts, workbook_part, Some(requested), "UniCell", None, None)?
            }
        };
        if let Some(comment) = existing {
            let reserved = [
                "ref",
                "id",
                "personId",
                "person",
                "author",
                "displayName",
                "text",
                "dateTime",
                "mentions",
            ];
            let mut changes = attribute_changes(object, &reserved)?;
            changes.push(("ref".to_string(), Some(reference.to_string())));
            changes.push(("personId".to_string(), Some(person_id)));
            if let Some(date_time) = object.get("dateTime").and_then(Value::as_str) {
                changes.push(("dT".to_string(), Some(date_time.to_string())));
            }
            xml = patch_open_tag(&xml, comment.range().start, &changes)?;
            if let Some(text) = object.get("text").and_then(Value::as_str) {
                let document =
                    Document::parse(&xml).map_err(|error| format!("{threaded_part}: {error}"))?;
                let comment = document
                    .descendants()
                    .find(|node| {
                        node.is_element()
                            && local_name(*node) == "threadedComment"
                            && requested_id
                                .map(|id| node.attribute("id") == Some(id))
                                .unwrap_or_else(|| node.attribute("ref") == Some(reference))
                    })
                    .ok_or_else(|| "threaded comment disappeared while patching".to_string())?;
                if let Some(text_node) = direct_child(comment, "text") {
                    let range = element_text_range(&xml, text_node)?;
                    xml.replace_range(range, &xml_escape_text(text));
                } else {
                    let prefix = qname_prefix(open_tag_qname(&xml, comment.range().start)?);
                    let qname = qualify(prefix, "text");
                    xml = insert_child_before_close(
                        &xml,
                        comment,
                        &format!("<{qname}>{}</{qname}>", xml_escape_text(text)),
                    )?;
                }
            }
            if let Some(mentions) = object.get("mentions") {
                let document =
                    Document::parse(&xml).map_err(|error| format!("{threaded_part}: {error}"))?;
                let comment = document
                    .descendants()
                    .find(|node| {
                        node.is_element()
                            && local_name(*node) == "threadedComment"
                            && requested_id
                                .map(|id| node.attribute("id") == Some(id))
                                .unwrap_or_else(|| node.attribute("ref") == Some(reference))
                    })
                    .ok_or_else(|| "threaded comment disappeared while patching".to_string())?;
                xml = patch_threaded_mentions(&xml, comment, mentions)?;
            }
        } else {
            let id = requested_id.map(str::to_string).unwrap_or_else(|| {
                unique_guid(&format!("comment:{sheet_part}:{reference}:{}", xml.len()))
            });
            let date_time = object
                .get("dateTime")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(current_rfc3339);
            let text = object
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let document = Document::parse(&xml).map_err(|error| error.to_string())?;
            let root = document.root_element();
            let prefix = qname_prefix(open_tag_qname(&xml, root.range().start)?);
            let comment_qname = qualify(prefix, "threadedComment");
            let text_qname = qualify(prefix, "text");
            let mut attributes = attribute_changes(
                object,
                &[
                    "ref",
                    "id",
                    "personId",
                    "person",
                    "author",
                    "displayName",
                    "text",
                    "dateTime",
                    "parentId",
                    "mentions",
                ],
            )?;
            attributes.retain(|(name, _)| {
                !["ref", "id", "personId", "dT", "parentId"].contains(&name.as_str())
            });
            attributes.push(("ref".to_string(), Some(reference.to_string())));
            attributes.push(("dT".to_string(), Some(date_time.clone())));
            attributes.push(("personId".to_string(), Some(person_id.clone())));
            attributes.push(("id".to_string(), Some(id.clone())));
            if let Some(parent) = object.get("parentId").and_then(Value::as_str) {
                attributes.push(("parentId".to_string(), Some(parent.to_string())));
            }
            let mentions = if let Some(values) = object.get("mentions") {
                let values = values
                    .as_array()
                    .ok_or_else(|| "threaded comment mentions must be an array".to_string())?;
                if values.is_empty() {
                    String::new()
                } else {
                    let mentions_qname = qualify(prefix, "mentions");
                    let mut children = String::new();
                    for value in values {
                        children.push_str(&mention_fragment(prefix, value)?);
                    }
                    format!("<{mentions_qname}>{children}</{mentions_qname}>")
                }
            } else {
                String::new()
            };
            let fragment = format!(
                "<{comment_qname}{}><{text_qname}>{}</{text_qname}>{mentions}</{comment_qname}>",
                attrs_fragment(&attributes),
                xml_escape_text(text)
            );
            xml = insert_child_before_close(&xml, root, &fragment)?;
        }
    }
    parts.insert(threaded_part, xml.into_bytes());
    Ok(())
}

fn source_from_rels_part(path: &str) -> Option<String> {
    if path == "_rels/.rels" {
        return Some(String::new());
    }
    let (prefix, file) = path.rsplit_once("/_rels/")?;
    let source_file = file.strip_suffix(".rels")?;
    Some(format!("{prefix}/{source_file}"))
}

fn content_type_for_part(
    parts: &BTreeMap<String, Vec<u8>>,
    part: &str,
) -> Result<Option<String>, String> {
    let path = "[Content_Types].xml";
    let xml = part_text(parts, path)?;
    let document = Document::parse(xml).map_err(|error| format!("{path}: {error}"))?;
    let root = document.root_element();
    let part_name = format!("/{}", normalize_part(part));
    if let Some(content_type) = direct_children(root, "Override")
        .find(|node| node.attribute("PartName") == Some(part_name.as_str()))
        .and_then(|node| node.attribute("ContentType"))
    {
        return Ok(Some(content_type.to_string()));
    }
    let extension = part.rsplit_once('.').map(|(_, extension)| extension);
    Ok(extension.and_then(|extension| {
        direct_children(root, "Default")
            .find(|node| node.attribute("Extension") == Some(extension))
            .and_then(|node| node.attribute("ContentType"))
            .map(str::to_string)
    }))
}

fn validate_package(
    parts: &BTreeMap<String, Vec<u8>>,
    changed_parts: &HashSet<String>,
) -> Result<(), String> {
    let content_types = part_text(parts, "[Content_Types].xml")?;
    let content_types_document =
        Document::parse(content_types).map_err(|error| format!("[Content_Types].xml: {error}"))?;
    if local_name(content_types_document.root_element()) != "Types"
        || content_types_document.root_element().tag_name().namespace() != Some(CONTENT_TYPES_NS)
    {
        return Err("[Content_Types].xml root must be Types".to_string());
    }
    for (path, bytes) in parts {
        if path.ends_with(".xml") || path.ends_with(".rels") || path.ends_with(".vml") {
            let xml = std::str::from_utf8(bytes)
                .map_err(|_| format!("changed XML part {path} is not UTF-8"))?;
            Document::parse(xml).map_err(|error| format!("{path}: {error}"))?;
        }
        if path.ends_with(".rels") {
            let Some(source_part) = source_from_rels_part(path) else {
                return Err(format!("invalid relationships part name {path}"));
            };
            for relationship in parse_relationships(parts, &source_part)? {
                if relationship
                    .target_mode
                    .as_deref()
                    .is_some_and(|mode| mode.eq_ignore_ascii_case("External"))
                {
                    continue;
                }
                let target = resolve_target(&source_part, &relationship.target);
                if !parts.contains_key(&target) {
                    return Err(format!(
                        "{path} relationship {} points to missing part {target}",
                        relationship.id
                    ));
                }
            }
        }
    }
    for part in changed_parts {
        if part == "[Content_Types].xml" || part.ends_with(".rels") || part == "_rels/.rels" {
            continue;
        }
        if content_type_for_part(parts, part)?.is_none() {
            return Err(format!("changed part {part} has no OPC content type"));
        }
    }
    let workbook_part = office_document_part(parts)?;
    let workbook_xml = part_text(parts, &workbook_part)?;
    let workbook_document =
        Document::parse(workbook_xml).map_err(|error| format!("{workbook_part}: {error}"))?;
    if workbook_document.root_element().tag_name().namespace() != Some(MAIN_NS)
        || local_name(workbook_document.root_element()) != "workbook"
    {
        return Err(format!("{workbook_part} is not a SpreadsheetML workbook"));
    }
    let workbook_relationship_ids: HashSet<String> = parse_relationships(parts, &workbook_part)?
        .into_iter()
        .map(|relationship| relationship.id)
        .collect();
    for node in workbook_document.descendants().filter(Node::is_element) {
        for attribute in node.attributes() {
            if attribute.namespace() == Some(REL_NS)
                && attribute.name() == "id"
                && !workbook_relationship_ids.contains(attribute.value())
            {
                return Err(format!(
                    "{workbook_part} references missing relationship {}",
                    attribute.value()
                ));
            }
        }
    }
    for sheet in workbook_sheets(parts, &workbook_part)? {
        let xml = part_text(parts, &sheet.part)?;
        let document = Document::parse(xml).map_err(|error| format!("{}: {error}", sheet.part))?;
        if document.root_element().tag_name().namespace() != Some(MAIN_NS)
            || local_name(document.root_element()) != "worksheet"
        {
            return Err(format!("{} is not a SpreadsheetML worksheet", sheet.part));
        }
        let relationship_ids: HashSet<String> = parse_relationships(parts, &sheet.part)?
            .into_iter()
            .map(|relationship| relationship.id)
            .collect();
        for node in document.descendants().filter(Node::is_element) {
            for attribute in node.attributes() {
                if attribute.namespace() == Some(REL_NS)
                    && attribute.name() == "id"
                    && !relationship_ids.contains(attribute.value())
                {
                    return Err(format!(
                        "{} references missing relationship {}",
                        sheet.part,
                        attribute.value()
                    ));
                }
            }
        }
    }
    Ok(())
}

fn validate_known_page_attributes(name: &str, patch: &Value) -> Result<(), String> {
    if delete_requested(patch) {
        return Ok(());
    }
    let object = patch
        .as_object()
        .ok_or_else(|| format!("{name} must be an object or null"))?;
    let attributes = object.get("attributes").and_then(Value::as_object);
    let get = |key: &str| {
        object
            .get(key)
            .or_else(|| attributes.and_then(|attrs| attrs.get(key)))
    };
    if name == "pageMargins" {
        for key in ["left", "right", "top", "bottom", "header", "footer"] {
            if let Some(value) = get(key) {
                if value.is_null() {
                    continue;
                }
                let number = value
                    .as_f64()
                    .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
                    .ok_or_else(|| format!("pageMargins.{key} must be numeric"))?;
                if !number.is_finite() || number < 0.0 {
                    return Err(format!(
                        "pageMargins.{key} must be a finite non-negative number"
                    ));
                }
            }
        }
    } else if name == "pageSetup" {
        if let Some(value) = get("paperSize") {
            if !value.is_null() {
                let paper_size = value
                    .as_u64()
                    .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
                    .ok_or_else(|| {
                        "pageSetup.paperSize must be an OOXML paper-size code".to_string()
                    })?;
                if paper_size == 0 || paper_size > 255 {
                    return Err("pageSetup.paperSize must be between 1 and 255".to_string());
                }
            }
        }
        if let Some(value) = get("orientation").and_then(Value::as_str) {
            if !["default", "portrait", "landscape"].contains(&value) {
                return Err(
                    "pageSetup.orientation must be default, portrait or landscape".to_string(),
                );
            }
        }
        if let Some(value) = get("pageOrder").and_then(Value::as_str) {
            if !["downThenOver", "overThenDown"].contains(&value) {
                return Err("pageSetup.pageOrder must be downThenOver or overThenDown".to_string());
            }
        }
        if let Some(value) = get("scale") {
            if !value.is_null() {
                let scale = value
                    .as_u64()
                    .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
                    .ok_or_else(|| "pageSetup.scale must be an unsigned integer".to_string())?;
                if !(10..=400).contains(&scale) {
                    return Err("pageSetup.scale must be between 10 and 400".to_string());
                }
            }
        }
    }
    Ok(())
}

fn normalize_page_setup_patch(patch: &Value) -> Result<Value, String> {
    if delete_requested(patch) {
        return Ok(patch.clone());
    }
    let mut object = patch
        .as_object()
        .cloned()
        .ok_or_else(|| "pageSetup must be an object or null".to_string())?;
    let map_name = |name: &str| -> Option<u64> {
        match name.trim().to_ascii_lowercase().as_str() {
            "letter" | "us-letter" => Some(1),
            "tabloid" => Some(3),
            "ledger" => Some(4),
            "legal" | "us-legal" => Some(5),
            "a3" => Some(8),
            "a4" => Some(9),
            "a5" => Some(11),
            "b4" | "b4-jis" => Some(12),
            "b5" | "b5-jis" => Some(13),
            "folio" => Some(14),
            _ => None,
        }
    };
    let normalize = |value: &Value| -> Result<Value, String> {
        if let Some(name) = value.as_str() {
            if let Ok(number) = name.parse::<u64>() {
                return Ok(Value::Number(number.into()));
            }
            return map_name(name)
                .map(|number| Value::Number(number.into()))
                .ok_or_else(|| format!("unknown paper size {name}"));
        }
        Ok(value.clone())
    };
    if let Some(value) = object.get("paperSize").cloned() {
        object.insert("paperSize".to_string(), normalize(&value)?);
    }
    if let Some(attributes) = object.get_mut("attributes") {
        let attributes = attributes
            .as_object_mut()
            .ok_or_else(|| "pageSetup.attributes must be an object".to_string())?;
        if let Some(value) = attributes.get("paperSize").cloned() {
            attributes.insert("paperSize".to_string(), normalize(&value)?);
        }
    }
    Ok(Value::Object(object))
}

fn worksheet_edits(request: &Map<String, Value>) -> Result<Vec<Value>, String> {
    if let Some(value) = request.get("worksheets") {
        return value
            .as_array()
            .cloned()
            .ok_or_else(|| "worksheets must be an array".to_string());
    }
    if let Some(value) = request.get("worksheet") {
        return Ok(vec![value.clone()]);
    }
    Ok(Vec::new())
}

/// Apply page-layout/review/protection changes transactionally and return the refreshed model.
///
/// Request shape:
///
/// ```text
/// {
///   workbookProtection: {...}|null,
///   definedNames: { upsert:[...], delete:[...], printArea:[...], printTitles:[...] },
///   worksheets: [{ sheet|sheetId|localSheetId|part, pageMargins, pageSetup, printOptions,
///                  headerFooter, rowBreaks, colBreaks, sheetProtection, protectedRanges,
///                  notes:{upsert,delete}, threadedComments:{upsert,deleteIds,deleteRefs} }]
/// }
/// ```
///
/// A `null` value (or `{ "$delete": true }`) removes the corresponding singleton. Missing
/// fields are untouched.  Attribute values can be provided directly or inside `attributes`.
pub(crate) fn apply_page_review_package_edit(
    parts: &mut BTreeMap<String, Vec<u8>>,
    request: &Value,
) -> Result<Value, String> {
    let request = request
        .as_object()
        .ok_or_else(|| "page/review edit request must be an object".to_string())?;
    let mut staged = parts.clone();
    let original_keys: HashSet<String> = staged.keys().cloned().collect();
    let workbook_part = office_document_part(&staged)?;
    let sheets = workbook_sheets(&staged, &workbook_part)?;

    if let Some(patch) = request.get("workbookProtection") {
        let xml = part_text(&staged, &workbook_part)?.to_string();
        let updated = patch_singleton(
            &xml,
            "workbookProtection",
            patch,
            WORKBOOK_ORDER,
            &[("lockStructure", "1")],
        )?;
        staged.insert(workbook_part.clone(), updated.into_bytes());
    }
    if let Some(patch) = request.get("definedNames") {
        let xml = part_text(&staged, &workbook_part)?.to_string();
        let updated = patch_defined_names(&xml, patch, &sheets)?;
        staged.insert(workbook_part.clone(), updated.into_bytes());
    }

    let edits = worksheet_edits(request)?;
    let mut print_upserts = Vec::new();
    for edit in edits {
        let edit = edit
            .as_object()
            .ok_or_else(|| "worksheet edit must be an object".to_string())?;
        let sheet = select_sheet(&sheets, edit)?;
        let mut xml = part_text(&staged, &sheet.part)?.to_string();
        if let Some(patch) = edit.get("pageMargins") {
            validate_known_page_attributes("pageMargins", patch)?;
            xml = patch_singleton(
                &xml,
                "pageMargins",
                patch,
                WORKSHEET_ORDER,
                &[
                    ("left", "0.7"),
                    ("right", "0.7"),
                    ("top", "0.75"),
                    ("bottom", "0.75"),
                    ("header", "0.3"),
                    ("footer", "0.3"),
                ],
            )?;
        }
        if let Some(patch) = edit.get("pageSetup") {
            let patch = normalize_page_setup_patch(patch)?;
            validate_known_page_attributes("pageSetup", &patch)?;
            xml = patch_singleton(&xml, "pageSetup", &patch, WORKSHEET_ORDER, &[])?;
        }
        if let Some(patch) = edit.get("printOptions") {
            xml = patch_singleton(&xml, "printOptions", patch, WORKSHEET_ORDER, &[])?;
        }
        if let Some(patch) = edit.get("headerFooter") {
            xml = patch_header_footer(&xml, patch)?;
        }
        if let Some(patch) = edit.get("rowBreaks") {
            xml = patch_breaks(&xml, "rowBreaks", patch)?;
        }
        if let Some(patch) = edit.get("colBreaks") {
            xml = patch_breaks(&xml, "colBreaks", patch)?;
        }
        if let Some(patch) = edit.get("sheetProtection") {
            xml = patch_singleton(
                &xml,
                "sheetProtection",
                patch,
                WORKSHEET_ORDER,
                &[("sheet", "1")],
            )?;
        }
        if let Some(patch) = edit.get("protectedRanges") {
            xml = patch_protected_ranges(&xml, patch)?;
        }
        staged.insert(sheet.part.clone(), xml.into_bytes());

        if let Some(patch) = edit.get("notes") {
            apply_notes_edit(&mut staged, &sheet.part, patch)?;
        }
        if let Some(patch) = edit.get("threadedComments") {
            apply_threaded_comments_edit(&mut staged, &workbook_part, &sheet.part, patch)?;
        }
        for (field, native_name) in [
            ("printArea", "_xlnm.Print_Area"),
            ("printTitles", "_xlnm.Print_Titles"),
        ] {
            if let Some(formula) = edit.get(field) {
                print_upserts.push(json!({
                    "name": native_name,
                    "localSheetId": sheet.local_sheet_id,
                    "formula": formula,
                }));
            }
        }
    }
    if !print_upserts.is_empty() {
        let xml = part_text(&staged, &workbook_part)?.to_string();
        let updated = patch_defined_names(&xml, &json!({"upsert": print_upserts}), &sheets)?;
        staged.insert(workbook_part.clone(), updated.into_bytes());
    }

    let changed_parts: HashSet<String> = staged
        .iter()
        .filter(|(path, bytes)| parts.get(*path) != Some(*bytes) || !original_keys.contains(*path))
        .map(|(path, _)| path.clone())
        .collect();
    validate_package(&staged, &changed_parts)?;
    *parts = staged;
    inspect_page_review_model(parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> BTreeMap<String, Vec<u8>> {
        BTreeMap::from([
            (
                "[Content_Types].xml".to_string(),
                format!(
                    r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="{CONTENT_TYPES_NS}"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#
                )
                .into_bytes(),
            ),
            (
                "_rels/.rels".to_string(),
                format!(
                    r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="{PACKAGE_REL_NS}"><Relationship Id="rId1" Type="{OFFICE_DOCUMENT_REL}" Target="xl/workbook.xml"/></Relationships>"#
                )
                .into_bytes(),
            ),
            (
                "xl/workbook.xml".to_string(),
                format!(
                    r#"<?xml version="1.0" encoding="UTF-8"?><workbook xmlns="{MAIN_NS}" xmlns:r="{REL_NS}" vendor="keep"><bookViews/><sheets><sheet name="Sheet 1" sheetId="1" r:id="rId1"/></sheets><calcPr calcId="191029"/><extLst><ext uri="vendor"><v:opaque xmlns:v="urn:vendor" x="1"/></ext></extLst></workbook>"#
                )
                .into_bytes(),
            ),
            (
                "xl/_rels/workbook.xml.rels".to_string(),
                format!(
                    r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="{PACKAGE_REL_NS}"><Relationship Id="rId1" Type="{WORKSHEET_REL}" Target="worksheets/sheet1.xml"/></Relationships>"#
                )
                .into_bytes(),
            ),
            (
                "xl/worksheets/sheet1.xml".to_string(),
                format!(
                    r#"<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="{MAIN_NS}" xmlns:r="{REL_NS}" vendor="keep"><sheetData><row r="1"><c r="A1"><v>1</v></c></row></sheetData><extLst><ext uri="vendor"><v:opaque xmlns:v="urn:vendor" x="1"/></ext></extLst></worksheet>"#
                )
                .into_bytes(),
            ),
        ])
    }

    fn text<'a>(parts: &'a BTreeMap<String, Vec<u8>>, part: &str) -> &'a str {
        std::str::from_utf8(parts.get(part).unwrap()).unwrap()
    }

    #[test]
    fn inspect_and_empty_edit_are_lossless() {
        let mut parts = fixture();
        let before = parts.clone();
        let model = inspect_page_review_model(&parts).unwrap();
        assert_eq!(model["worksheets"][0]["name"], "Sheet 1");
        assert!(model["worksheets"][0]["pageMargins"].is_null());
        apply_page_review_package_edit(&mut parts, &json!({})).unwrap();
        assert_eq!(parts, before);
    }

    #[test]
    fn creates_and_edits_layout_print_names_and_protection() {
        let mut parts = fixture();
        let model = apply_page_review_package_edit(
            &mut parts,
            &json!({
                "workbookProtection": {"lockStructure": true, "vendorLock": "keep-me"},
                "worksheets": [{
                    "sheetId": 1,
                    "pageMargins": {"left": 0.25, "right": 0.25},
                    "pageSetup": {"paperSize": "A4", "orientation": "landscape", "fitToWidth": 1, "fitToHeight": 0},
                    "printOptions": {"gridLines": true, "horizontalCentered": true},
                    "headerFooter": {"differentFirst": true, "oddHeader": "&CQuarter &A", "firstFooter": "Page &P"},
                    "rowBreaks": [{"id": 20, "min": 0, "max": 16383, "man": true}],
                    "colBreaks": [{"id": 4, "min": 0, "max": 1048575, "man": true}],
                    "sheetProtection": {"sheet": true, "objects": false, "algorithmName": "SHA-512", "hashValue": "abc"},
                    "protectedRanges": [{"name": "Input", "sqref": "A1:B5", "securityDescriptor": "opaque"}],
                    "printArea": "'Sheet 1'!$A$1:$F$20",
                    "printTitles": "'Sheet 1'!$1:$2"
                }]
            }),
        )
        .unwrap();
        assert_eq!(model["workbookProtection"]["vendorLock"], "keep-me");
        assert_eq!(model["worksheets"][0]["pageSetup"]["paperSize"], "9");
        assert_eq!(
            model["worksheets"][0]["headerFooter"]["oddHeader"],
            "&CQuarter &A"
        );
        assert_eq!(
            model["worksheets"][0]["rowBreaks"]["attributes"]["count"],
            "1"
        );
        assert_eq!(
            model["worksheets"][0]["protectedRanges"][0]["securityDescriptor"],
            "opaque"
        );
        assert_eq!(model["definedNames"].as_array().unwrap().len(), 2);
        let workbook = text(&parts, "xl/workbook.xml");
        assert!(
            workbook.find("<workbookProtection").unwrap() < workbook.find("<bookViews").unwrap()
        );
        assert!(workbook.contains("<v:opaque xmlns:v=\"urn:vendor\" x=\"1\"/>"));
        let sheet = text(&parts, "xl/worksheets/sheet1.xml");
        assert!(sheet.find("<printOptions").unwrap() < sheet.find("<pageMargins").unwrap());
        assert!(sheet.find("<pageMargins").unwrap() < sheet.find("<pageSetup").unwrap());
        assert!(sheet.contains("vendor=\"keep\""));
        assert!(sheet.contains("<v:opaque xmlns:v=\"urn:vendor\" x=\"1\"/>"));

        apply_page_review_package_edit(
            &mut parts,
            &json!({"worksheets":[{"sheet":"Sheet 1","headerFooter":{"oddHeader":null},"rowBreaks":null,"sheetProtection":null,"printArea":null}]}),
        )
        .unwrap();
        let model = inspect_page_review_model(&parts).unwrap();
        assert!(model["worksheets"][0]["headerFooter"]["oddHeader"].is_null());
        assert!(model["worksheets"][0]["rowBreaks"].is_null());
        assert!(model["worksheets"][0]["sheetProtection"].is_null());
        assert_eq!(
            model["definedNames"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|value| value["kind"] == "printArea")
                .count(),
            0
        );
    }

    #[test]
    fn creates_updates_and_deletes_legacy_notes_with_vml() {
        let mut parts = fixture();
        let created = apply_page_review_package_edit(
            &mut parts,
            &json!({"worksheets":[{"sheetId":1,"notes":{"upsert":[
                {"ref":"B3","author":"Alice","text":"first"},
                {"ref":"C4","author":"Bob","text":"second"}
            ]}}]}),
        )
        .unwrap();
        assert_eq!(
            created["worksheets"][0]["notes"]["items"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        let comments_part = created["worksheets"][0]["notes"]["part"].as_str().unwrap();
        let comments = text(&parts, comments_part);
        assert!(comments.contains("<author>Alice</author>"));
        assert!(comments.contains("ref=\"B3\""));
        let vml_part = relationship_target(&parts, "xl/worksheets/sheet1.xml", VML_REL)
            .unwrap()
            .unwrap()
            .1;
        assert!(
            text(&parts, &vml_part)
                .matches("ObjectType=\"Note\"")
                .count()
                == 2
        );
        assert!(text(&parts, "xl/worksheets/sheet1.xml").contains("legacyDrawing"));

        let comments_with_vendor_run = comments.replace(
            "<text><t xml:space=\"preserve\">first</t></text>",
            "<text vendor=\"keep\"><r><rPr><b/><v:x xmlns:v=\"urn:vendor\"/></rPr><t>fi</t></r><r><t>rst</t></r></text>",
        );
        parts.insert(
            comments_part.to_string(),
            comments_with_vendor_run.into_bytes(),
        );
        apply_page_review_package_edit(
            &mut parts,
            &json!({"worksheets":[{"sheetId":1,"notes":{"upsert":[{"ref":"B3","text":"changed"}],"delete":["C4"]}}]}),
        )
        .unwrap();
        let comments = text(&parts, comments_part);
        assert!(comments.contains("vendor=\"keep\""));
        assert!(comments.contains("<b/>"));
        assert!(comments.contains("<v:x xmlns:v=\"urn:vendor\"/>"));
        assert!(comments.contains("<t>ch</t>"));
        assert!(comments.contains("<t>anged</t>"));
        assert!(!comments.contains("ref=\"C4\""));
        assert_eq!(
            text(&parts, &vml_part)
                .matches("ObjectType=\"Note\"")
                .count(),
            1
        );
    }

    #[test]
    fn creates_and_round_trips_threaded_comments_and_persons() {
        let mut parts = fixture();
        let model = apply_page_review_package_edit(
            &mut parts,
            &json!({"worksheets":[{"sheetId":1,"threadedComments":{"upsert":[{
                "ref":"D5","author":"Chen","text":"hello","dateTime":"2026-08-05T01:02:03Z"
            }]}}]}),
        )
        .unwrap();
        let item = &model["worksheets"][0]["threadedComments"]["items"][0];
        assert_eq!(item["ref"], "D5");
        assert_eq!(item["text"], "hello");
        assert_eq!(item["person"]["displayName"], "Chen");
        let id = item["id"].as_str().unwrap().to_string();
        let person_id = item["personId"].as_str().unwrap().to_string();
        let threaded_part = model["worksheets"][0]["threadedComments"]["part"]
            .as_str()
            .unwrap()
            .to_string();
        let threaded = text(&parts, &threaded_part)
            .replace(
                "<text>hello</text>",
                "<text>hello</text><vendor:opaque xmlns:vendor=\"urn:vendor\" keep=\"1\"/>",
            )
            .replace(" ref=\"D5\"", " vendor=\"keep\" ref=\"D5\"");
        parts.insert(threaded_part.clone(), threaded.into_bytes());
        apply_page_review_package_edit(
            &mut parts,
            &json!({"worksheets":[{"sheetId":1,"threadedComments":{"upsert":[{
                "id":id,"ref":"D5","text":"updated","author":"Chen",
                "mentions":[{"mentionId":"m1","personId":person_id,"startIndex":0,"length":4}]
            }]}}]}),
        )
        .unwrap();
        let threaded = text(&parts, &threaded_part);
        assert!(threaded.contains("vendor=\"keep\""));
        assert!(threaded.contains("<vendor:opaque xmlns:vendor=\"urn:vendor\" keep=\"1\"/>"));
        assert!(threaded.contains("<text>updated</text>"));
        assert!(threaded.contains("mentionId=\"m1\""));
        assert_eq!(
            inspect_page_review_model(&parts).unwrap()["worksheets"][0]["threadedComments"]
                ["items"][0]["mentions"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        apply_page_review_package_edit(
            &mut parts,
            &json!({"worksheets":[{"sheetId":1,"threadedComments":{"deleteIds":[id]}}]}),
        )
        .unwrap();
        assert!(inspect_page_review_model(&parts).unwrap()["worksheets"][0]["threadedComments"]
            ["items"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn invalid_edit_is_transactional() {
        let mut parts = fixture();
        let before = parts.clone();
        let error = apply_page_review_package_edit(
            &mut parts,
            &json!({"worksheets":[{"sheetId":1,"pageSetup":{"orientation":"diagonal"}}]}),
        )
        .unwrap_err();
        assert!(error.contains("orientation"));
        assert_eq!(parts, before);
    }

    #[test]
    fn differential_break_and_range_edits_preserve_vendor_payloads() {
        let mut parts = fixture();
        let sheet = text(&parts, "xl/worksheets/sheet1.xml").replace(
            "<extLst>",
            "<protectedRanges vendor=\"root\"><protectedRange name=\"Input\" sqref=\"A1\" vendor=\"item\"><v:acl xmlns:v=\"urn:vendor\"/></protectedRange><v:opaque xmlns:v=\"urn:vendor\"/></protectedRanges><rowBreaks count=\"1\" manualBreakCount=\"1\" vendor=\"root\"><brk id=\"5\" min=\"0\" max=\"10\" man=\"1\" vendor=\"item\"/><v:opaque xmlns:v=\"urn:vendor\"/></rowBreaks><extLst>",
        );
        parts.insert("xl/worksheets/sheet1.xml".to_string(), sheet.into_bytes());
        apply_page_review_package_edit(
            &mut parts,
            &json!({"worksheets":[{"sheetId":1,
                "protectedRanges":{"upsert":[{"name":"Input","sqref":"B2:C3"}]},
                "rowBreaks":{"upsert":[{"id":5,"max":20}]}
            }]}),
        )
        .unwrap();
        let sheet = text(&parts, "xl/worksheets/sheet1.xml");
        assert!(sheet.contains("<protectedRanges vendor=\"root\">"));
        assert!(sheet.contains("name=\"Input\" sqref=\"B2:C3\" vendor=\"item\""));
        assert!(sheet.contains("<v:acl xmlns:v=\"urn:vendor\"/>"));
        assert!(sheet.contains("<rowBreaks count=\"1\" manualBreakCount=\"1\" vendor=\"root\">"));
        assert!(sheet.contains("id=\"5\" min=\"0\" max=\"20\" man=\"1\" vendor=\"item\""));
        assert_eq!(
            sheet.matches("<v:opaque xmlns:v=\"urn:vendor\"/>").count(),
            2
        );

        apply_page_review_package_edit(
            &mut parts,
            &json!({"worksheets":[{"sheetId":1,
                "protectedRanges":{"deleteNames":["Input"]},
                "rowBreaks":{"deleteIds":[5]}
            }]}),
        )
        .unwrap();
        let sheet = text(&parts, "xl/worksheets/sheet1.xml");
        assert!(sheet.contains("<protectedRanges vendor=\"root\"><v:opaque"));
        assert!(
            sheet.contains(
                "<rowBreaks count=\"0\" manualBreakCount=\"0\" vendor=\"root\"><v:opaque"
            )
        );
    }

    #[test]
    fn broken_relationship_is_rejected_without_commit() {
        let mut parts = fixture();
        let path = "xl/worksheets/_rels/sheet1.xml.rels";
        parts.insert(
            path.to_string(),
            format!(
                r#"<Relationships xmlns="{PACKAGE_REL_NS}"><Relationship Id="rId9" Type="{COMMENTS_REL}" Target="../missing.xml"/></Relationships>"#
            )
            .into_bytes(),
        );
        let before = parts.clone();
        let error = apply_page_review_package_edit(
            &mut parts,
            &json!({"worksheets":[{"sheetId":1,"pageMargins":{"left":1.0}}]}),
        )
        .unwrap_err();
        assert!(error.contains("missing part"));
        assert_eq!(parts, before);
    }
}
