//! Native external-data metadata and a deterministic, sandboxed Power Query subset.
//!
//! The OOXML half deliberately treats the workbook as an OPC graph. It resolves relationships
//! instead of relying on Excel's conventional filenames, performs byte-range edits on the small
//! set of attributes that UniCell owns, and leaves Mashup/VertiPaq/custom-data payloads untouched.
//! The M evaluator is intentionally not a general Power Query host: it accepts only in-memory,
//! CSV, or JSON values supplied by the caller and rejects every network/file/provider primitive.

use roxmltree::{Document, Node};
use serde_json::{Map, Number, Value, json};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ops::Range;

#[derive(Clone, Debug)]
struct Relationship {
    owner: String,
    relationship_part: String,
    id: String,
    kind: String,
    target: String,
    target_mode: Option<String>,
    resolved_part: Option<String>,
}

#[derive(Clone, Debug)]
struct AttributeSpan {
    name: String,
    value_range: Range<usize>,
    full_range: Range<usize>,
}

fn normalize_part(path: &str) -> Option<String> {
    let replaced = path.replace('\\', "/");
    let mut segments = Vec::new();
    for segment in replaced.trim_start_matches('/').split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop()?;
            }
            value => segments.push(value),
        }
    }
    (!segments.is_empty()).then(|| segments.join("/"))
}

fn relationship_owner(path: &str) -> Option<String> {
    if path == "_rels/.rels" {
        return Some(String::new());
    }
    let (directory, filename) = path.rsplit_once("/_rels/")?;
    let filename = filename.strip_suffix(".rels")?;
    normalize_part(&format!("{directory}/{filename}"))
}

fn resolve_target(owner: &str, target: &str) -> Option<String> {
    if target.starts_with('/') {
        return normalize_part(target);
    }
    let directory = owner.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("");
    if directory.is_empty() {
        normalize_part(target)
    } else {
        normalize_part(&format!("{directory}/{target}"))
    }
}

fn parse_xml<'a>(
    parts: &'a BTreeMap<String, Vec<u8>>,
    part: &str,
) -> Result<(&'a str, Document<'a>), String> {
    let bytes = parts
        .get(part)
        .ok_or_else(|| format!("missing OPC part {part}"))?;
    let xml =
        std::str::from_utf8(bytes).map_err(|error| format!("{part} is not UTF-8 XML: {error}"))?;
    let document =
        Document::parse(xml).map_err(|error| format!("invalid XML in {part}: {error}"))?;
    Ok((xml, document))
}

fn all_relationships(parts: &BTreeMap<String, Vec<u8>>) -> Result<Vec<Relationship>, String> {
    let mut output = Vec::new();
    for (part, bytes) in parts {
        let Some(owner) = relationship_owner(part) else {
            continue;
        };
        let xml = std::str::from_utf8(bytes)
            .map_err(|error| format!("{part} is not UTF-8 XML: {error}"))?;
        let document = Document::parse(xml)
            .map_err(|error| format!("invalid relationships XML in {part}: {error}"))?;
        for node in document
            .descendants()
            .filter(|node| node.is_element() && node.tag_name().name() == "Relationship")
        {
            let target = node.attribute("Target").unwrap_or("").to_string();
            let target_mode = node.attribute("TargetMode").map(str::to_string);
            let external = target_mode
                .as_deref()
                .is_some_and(|value| value.eq_ignore_ascii_case("External"));
            output.push(Relationship {
                owner: owner.clone(),
                relationship_part: part.clone(),
                id: node.attribute("Id").unwrap_or("").to_string(),
                kind: node.attribute("Type").unwrap_or("").to_string(),
                target: target.clone(),
                target_mode,
                resolved_part: (!external)
                    .then(|| resolve_target(&owner, &target))
                    .flatten(),
            });
        }
    }
    output.sort_by(|left, right| {
        left.owner
            .cmp(&right.owner)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(output)
}

fn content_types(parts: &BTreeMap<String, Vec<u8>>) -> Result<HashMap<String, String>, String> {
    let (_, document) = parse_xml(parts, "[Content_Types].xml")?;
    let mut defaults = HashMap::new();
    let mut overrides = HashMap::new();
    for node in document.root_element().children().filter(Node::is_element) {
        match node.tag_name().name() {
            "Default" => {
                if let (Some(extension), Some(kind)) =
                    (node.attribute("Extension"), node.attribute("ContentType"))
                {
                    defaults.insert(extension.to_ascii_lowercase(), kind.to_string());
                }
            }
            "Override" => {
                if let (Some(name), Some(kind)) =
                    (node.attribute("PartName"), node.attribute("ContentType"))
                {
                    if let Some(part) = normalize_part(name) {
                        overrides.insert(part, kind.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    for part in parts.keys() {
        if overrides.contains_key(part) {
            continue;
        }
        if let Some(extension) = part
            .rsplit_once('.')
            .map(|(_, extension)| extension.to_ascii_lowercase())
        {
            if let Some(kind) = defaults.get(&extension) {
                overrides.insert(part.clone(), kind.clone());
            }
        }
    }
    Ok(overrides)
}

fn attr_map(node: Node<'_, '_>) -> Map<String, Value> {
    node.attributes()
        .map(|attribute| {
            (
                attribute.name().to_string(),
                Value::String(attribute.value().to_string()),
            )
        })
        .collect()
}

fn direct_child<'a>(node: Node<'a, 'a>, name: &str) -> Option<Node<'a, 'a>> {
    node.children()
        .find(|child| child.is_element() && child.tag_name().name() == name)
}

fn direct_children<'a>(
    node: Node<'a, 'a>,
    name: &'a str,
) -> impl Iterator<Item = Node<'a, 'a>> + 'a {
    node.children()
        .filter(move |child| child.is_element() && child.tag_name().name() == name)
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn redacted_connection(value: &str) -> (String, bool) {
    let mut changed = false;
    let output = value
        .split(';')
        .map(|item| {
            let Some((key, _)) = item.split_once('=') else {
                return item.to_string();
            };
            let lower = key.trim().to_ascii_lowercase();
            if ["password", "pwd", "token", "access token", "credential"]
                .iter()
                .any(|needle| lower.contains(needle))
            {
                changed = true;
                format!("{key}=***")
            } else {
                item.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(";");
    (output, changed)
}

fn relation_json(relation: &Relationship) -> Value {
    json!({
        "owner": relation.owner,
        "relationshipPart": relation.relationship_part,
        "id": relation.id,
        "type": relation.kind,
        "target": relation.target,
        "targetMode": relation.target_mode,
        "resolvedPart": relation.resolved_part,
    })
}

fn inspect_connections(
    parts: &BTreeMap<String, Vec<u8>>,
    part: &str,
) -> Result<Vec<Value>, String> {
    let (_, document) = parse_xml(parts, part)?;
    let mut output = Vec::new();
    for connection in document
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "connection")
    {
        let mut source = Value::Null;
        let mut command_text = None;
        let mut source_redacted = false;
        for child in connection.children().filter(Node::is_element) {
            match child.tag_name().name() {
                "dbPr" => {
                    let raw = child.attribute("connection").unwrap_or("");
                    let (safe, redacted) = redacted_connection(raw);
                    source_redacted |= redacted;
                    command_text = child.attribute("command").map(str::to_string);
                    source = json!({"kind":"database", "connection":safe, "commandType":child.attribute("commandType"), "attributes":attr_map(child)});
                    if redacted {
                        if let Some(attributes) =
                            source.get_mut("attributes").and_then(Value::as_object_mut)
                        {
                            attributes.insert("connection".to_string(), Value::String(safe));
                        }
                    }
                }
                "webPr" => {
                    source = json!({"kind":"web", "url":child.attribute("url"), "attributes":attr_map(child)})
                }
                "textPr" => {
                    source = json!({"kind":"text", "sourceFile":child.attribute("sourceFile"), "attributes":attr_map(child)})
                }
                "olapPr" => source = json!({"kind":"olap", "attributes":attr_map(child)}),
                _ => {}
            }
        }
        output.push(json!({
            "part": part,
            "id": connection.attribute("id"),
            "name": connection.attribute("name"),
            "description": connection.attribute("description"),
            "type": connection.attribute("type"),
            "attributes": attr_map(connection),
            "source": source,
            "sourceRedacted": source_redacted,
            "commandText": command_text,
            "parameters": direct_child(connection, "parameters").map(|parameters| direct_children(parameters, "parameter").map(|node| Value::Object(attr_map(node))).collect::<Vec<_>>()).unwrap_or_default(),
            "hasExtensions": direct_child(connection, "extLst").is_some(),
        }));
    }
    Ok(output)
}

fn inspect_query_table(
    parts: &BTreeMap<String, Vec<u8>>,
    part: &str,
    relationships: &[Relationship],
) -> Result<Value, String> {
    let (_, document) = parse_xml(parts, part)?;
    let root = document.root_element();
    let refresh = direct_child(root, "queryTableRefresh");
    let fields_parent = refresh.and_then(|node| direct_child(node, "queryTableFields"));
    let fields = fields_parent
        .map(|parent| {
            direct_children(parent, "queryTableField")
                .map(|field| Value::Object(attr_map(field)))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut linked_tables = Vec::new();
    for relationship in relationships.iter().filter(|relationship| {
        relationship.resolved_part.as_deref() == Some(part)
            && relationship
                .kind
                .to_ascii_lowercase()
                .contains("querytable")
    }) {
        if let Some(table_bytes) = parts.get(&relationship.owner) {
            if let Ok(table_xml) = std::str::from_utf8(table_bytes) {
                if let Ok(table_document) = Document::parse(table_xml) {
                    let table_root = table_document.root_element();
                    linked_tables.push(json!({"part":relationship.owner, "relationshipId":relationship.id, "loadRange":table_root.attribute("ref"), "name":table_root.attribute("displayName").or_else(|| table_root.attribute("name"))}));
                }
            }
        }
    }
    Ok(json!({
        "part": part,
        "name": root.attribute("name"),
        "connectionId": root.attribute("connectionId"),
        "attributes": attr_map(root),
        "refresh": refresh.map(attr_map),
        "fields": fields,
        "linkedTables": linked_tables,
        "hasExtensions": direct_child(root, "extLst").is_some() || refresh.and_then(|node| direct_child(node, "extLst")).is_some(),
    }))
}

fn inspect_external_link(
    parts: &BTreeMap<String, Vec<u8>>,
    part: &str,
    relationships: &[Relationship],
) -> Result<Value, String> {
    let (_, document) = parse_xml(parts, part)?;
    let root = document.root_element();
    let sheet_names = document
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "sheetName")
        .filter_map(|node| node.attribute("val"))
        .map(Value::from)
        .collect::<Vec<_>>();
    let defined_names = document.descendants().filter(|node| node.is_element() && node.tag_name().name() == "definedName").map(|node| json!({"name":node.attribute("name"), "refersTo":node.attribute("refersTo"), "sheetId":node.attribute("sheetId")})).collect::<Vec<_>>();
    let cached_cells = document
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "cell")
        .count();
    let outgoing = relationships
        .iter()
        .filter(|relationship| relationship.owner == part)
        .map(relation_json)
        .collect::<Vec<_>>();
    Ok(
        json!({"part":part, "attributes":attr_map(root), "sheetNames":sheet_names, "definedNames":defined_names, "cachedCellCount":cached_cells, "relationships":outgoing}),
    )
}

fn is_connection_part(part: &str, kind: Option<&str>, incoming: &[&Relationship]) -> bool {
    part.to_ascii_lowercase().ends_with("connections.xml")
        || kind.is_some_and(|value| value.to_ascii_lowercase().contains("connections"))
        || incoming.iter().any(|relationship| {
            relationship
                .kind
                .to_ascii_lowercase()
                .ends_with("/connections")
        })
}

fn is_query_table_part(part: &str, kind: Option<&str>, incoming: &[&Relationship]) -> bool {
    part.to_ascii_lowercase().contains("/querytables/")
        || kind.is_some_and(|value| value.to_ascii_lowercase().contains("querytable"))
        || incoming.iter().any(|relationship| {
            relationship
                .kind
                .to_ascii_lowercase()
                .contains("querytable")
        })
}

fn is_external_link_part(part: &str, kind: Option<&str>, incoming: &[&Relationship]) -> bool {
    part.to_ascii_lowercase().contains("/externallinks/") && part.ends_with(".xml")
        || kind.is_some_and(|value| value.to_ascii_lowercase().contains("externallink"))
        || incoming.iter().any(|relationship| {
            relationship
                .kind
                .to_ascii_lowercase()
                .contains("externallink")
        })
}

fn is_opaque_data_part(part: &str, kind: Option<&str>, incoming: &[&Relationship]) -> bool {
    let path = part.to_ascii_lowercase();
    let content = kind.unwrap_or("").to_ascii_lowercase();
    let relation_kind = incoming
        .iter()
        .map(|relationship| relationship.kind.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    [
        "/model/",
        "/customdata/",
        "mashup",
        "powerpivot",
        "vertipaq",
    ]
    .iter()
    .any(|needle| path.contains(needle))
        || [
            "excel.model",
            "customdata",
            "mashup",
            "powerpivot",
            "datamodel",
        ]
        .iter()
        .any(|needle| content.contains(needle) || relation_kind.contains(needle))
}

/// Inspects the native external-data graph without decoding or rewriting opaque payloads.
pub(crate) fn inspect_native_data(parts: &BTreeMap<String, Vec<u8>>) -> Result<Value, String> {
    let relationships = all_relationships(parts)?;
    let types = content_types(parts)?;
    let mut connections = Vec::new();
    let mut query_tables = Vec::new();
    let mut external_links = Vec::new();
    let mut opaque_parts = Vec::new();
    let mut roots = BTreeSet::new();
    let mut warnings = Vec::new();
    for (part, bytes) in parts {
        if part.ends_with(".rels") || part == "[Content_Types].xml" {
            continue;
        }
        let incoming = relationships
            .iter()
            .filter(|relationship| relationship.resolved_part.as_deref() == Some(part))
            .collect::<Vec<_>>();
        let content_type = types.get(part).map(String::as_str);
        if is_connection_part(part, content_type, &incoming) {
            roots.insert(part.clone());
            match inspect_connections(parts, part) {
                Ok(items) => connections.extend(items),
                Err(error) => warnings.push(error),
            }
        } else if is_query_table_part(part, content_type, &incoming) {
            roots.insert(part.clone());
            match inspect_query_table(parts, part, &relationships) {
                Ok(item) => query_tables.push(item),
                Err(error) => warnings.push(error),
            }
        } else if is_external_link_part(part, content_type, &incoming) {
            roots.insert(part.clone());
            match inspect_external_link(parts, part, &relationships) {
                Ok(item) => external_links.push(item),
                Err(error) => warnings.push(error),
            }
        } else if is_opaque_data_part(part, content_type, &incoming) {
            roots.insert(part.clone());
            opaque_parts.push(json!({"part":part, "contentType":content_type, "size":bytes.len(), "sha256":sha256(bytes), "opaque":true}));
        }
    }
    // Keep all incoming/outgoing dependencies for the discovered graph, including external URI
    // relationships and relationship parts themselves. This makes dependency-preserving copy easy.
    let mut closure = roots.clone();
    loop {
        let before = closure.len();
        for relation in &relationships {
            if closure.contains(&relation.owner)
                || relation
                    .resolved_part
                    .as_ref()
                    .is_some_and(|part| closure.contains(part))
            {
                if !relation.owner.is_empty() {
                    closure.insert(relation.owner.clone());
                }
                if let Some(part) = &relation.resolved_part {
                    closure.insert(part.clone());
                }
            }
        }
        if before == closure.len() {
            break;
        }
    }
    let dependencies = relationships
        .iter()
        .filter(|relationship| {
            closure.contains(&relationship.owner)
                || relationship
                    .resolved_part
                    .as_ref()
                    .is_some_and(|part| closure.contains(part))
        })
        .map(relation_json)
        .collect::<Vec<_>>();
    let data_model_relationships = relationships
        .iter()
        .filter(|relationship| {
            let kind = relationship.kind.to_ascii_lowercase();
            let target = relationship.target.to_ascii_lowercase();
            ["model", "customdata", "mashup", "powerpivot", "datamodel"]
                .iter()
                .any(|needle| kind.contains(needle) || target.contains(needle))
        })
        .map(relation_json)
        .collect::<Vec<_>>();
    Ok(json!({
        "connections": connections,
        "queryTables": query_tables,
        "externalLinks": external_links,
        "opaqueDataParts": opaque_parts.clone(),
        "dataModel": {"parts":opaque_parts, "relationships":data_model_relationships, "executable":false, "daxRuntime":false},
        "dependencies": dependencies,
        "warnings": warnings,
        "capabilities": {"mashupPreservation":"opaque-byte-exact", "vertiPaqPreservation":"opaque-byte-exact", "mRuntime":"safe-subset", "daxRuntime":false},
    }))
}

fn scan_open_tag_end(xml: &str, start: usize) -> Result<usize, String> {
    let bytes = xml.as_bytes();
    if bytes.get(start) != Some(&b'<') {
        return Err("element does not start with '<'".to_string());
    }
    let mut cursor = start + 1;
    let mut quote = None;
    while cursor < bytes.len() {
        match (quote, bytes[cursor]) {
            (Some(current), value) if current == value => quote = None,
            (None, b'\'' | b'"') => quote = Some(bytes[cursor]),
            (None, b'>') => return Ok(cursor + 1),
            _ => {}
        }
        cursor += 1;
    }
    Err("unterminated XML start tag".to_string())
}

fn scan_attributes(tag: &str) -> Result<Vec<AttributeSpan>, String> {
    let bytes = tag.as_bytes();
    let mut cursor = 1usize;
    while cursor < bytes.len()
        && !bytes[cursor].is_ascii_whitespace()
        && !matches!(bytes[cursor], b'/' | b'>')
    {
        cursor += 1;
    }
    let mut output = Vec::new();
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
            _ => return Err("XML attribute is not quoted".to_string()),
        };
        cursor += 1;
        let value_start = cursor;
        while cursor < bytes.len() && bytes[cursor] != quote {
            cursor += 1;
        }
        if cursor >= bytes.len() {
            return Err("unterminated XML attribute".to_string());
        }
        let value_end = cursor;
        cursor += 1;
        output.push(AttributeSpan {
            name: tag[name_start..name_end].to_string(),
            value_range: value_start..value_end,
            full_range: whitespace_start..cursor,
        });
    }
    Ok(output)
}

fn escape_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn attribute_value(value: &Value) -> Result<Option<String>, String> {
    Ok(match value {
        Value::Null => None,
        Value::String(value) => Some(value.clone()),
        Value::Bool(value) => Some(if *value { "1" } else { "0" }.to_string()),
        Value::Number(value) => Some(value.to_string()),
        _ => return Err("XML attribute values must be string/number/bool/null".to_string()),
    })
}

fn local_attribute_name(name: &str) -> &str {
    name.rsplit_once(':').map(|(_, name)| name).unwrap_or(name)
}

fn patch_open_tag(
    xml: &str,
    node: Node<'_, '_>,
    patch: &Map<String, Value>,
    allowed: &[&str],
) -> Result<String, String> {
    for key in patch.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(format!(
                "attribute {key} is not editable on {}",
                node.tag_name().name()
            ));
        }
    }
    if patch.is_empty() {
        return Ok(xml.to_string());
    }
    let start = node.range().start;
    let end = scan_open_tag_end(xml, start)?;
    let tag = &xml[start..end];
    let spans = scan_attributes(tag)?;
    let mut output_tag = tag.to_string();
    let mut replacements = Vec::new();
    let mut seen = HashSet::new();
    for span in spans {
        let local = local_attribute_name(&span.name);
        let Some(value) = patch.get(local) else {
            continue;
        };
        seen.insert(local.to_string());
        match attribute_value(value)? {
            Some(value) => replacements.push((span.value_range, escape_attribute(&value))),
            None => replacements.push((span.full_range, String::new())),
        }
    }
    replacements.sort_by(|left, right| right.0.start.cmp(&left.0.start));
    for (range, value) in replacements {
        output_tag.replace_range(range, &value);
    }
    let insertion = output_tag
        .rfind("/>")
        .or_else(|| output_tag.rfind('>'))
        .ok_or_else(|| "invalid XML start tag".to_string())?;
    let mut additions = String::new();
    for (key, value) in patch {
        if seen.contains(key) {
            continue;
        }
        if let Some(value) = attribute_value(value)? {
            additions.push_str(&format!(" {key}=\"{}\"", escape_attribute(&value)));
        }
    }
    output_tag.insert_str(insertion, &additions);
    let mut output = xml.to_string();
    output.replace_range(start..end, &output_tag);
    Ok(output)
}

fn find_element<'a>(
    document: &'a Document<'a>,
    name: &str,
    key: Option<(&str, &str)>,
) -> Option<Node<'a, 'a>> {
    document.descendants().find(|node| {
        node.is_element()
            && node.tag_name().name() == name
            && key.is_none_or(|(attribute, value)| node.attribute(attribute) == Some(value))
    })
}

fn patch_element_attributes(
    xml: &str,
    name: &str,
    key: Option<(&str, &str)>,
    patch: &Map<String, Value>,
    allowed: &[&str],
) -> Result<String, String> {
    let document = Document::parse(xml).map_err(|error| format!("invalid XML: {error}"))?;
    let node = find_element(&document, name, key).ok_or_else(|| match key {
        Some((attribute, value)) => format!("missing {name} with {attribute}={value}"),
        None => format!("missing {name}"),
    })?;
    patch_open_tag(xml, node, patch, allowed)
}

fn qname_prefix(xml: &str, node: Node<'_, '_>) -> Result<String, String> {
    let start = node.range().start + 1;
    let end = scan_open_tag_end(xml, node.range().start)?;
    let bytes = xml.as_bytes();
    let mut cursor = start;
    while cursor < end
        && !bytes[cursor].is_ascii_whitespace()
        && !matches!(bytes[cursor], b'/' | b'>')
    {
        cursor += 1;
    }
    let qname = &xml[start..cursor];
    Ok(qname
        .rsplit_once(':')
        .map(|(prefix, _)| format!("{prefix}:"))
        .unwrap_or_default())
}

fn child_of<'a>(parent: Node<'a, 'a>, name: &str) -> Option<Node<'a, 'a>> {
    parent
        .children()
        .find(|node| node.is_element() && node.tag_name().name() == name)
}

fn patch_or_insert_child(
    xml: &str,
    parent_name: &str,
    parent_key: Option<(&str, &str)>,
    child_name: &str,
    patch: &Map<String, Value>,
    allowed: &[&str],
) -> Result<String, String> {
    let document = Document::parse(xml).map_err(|error| format!("invalid XML: {error}"))?;
    let parent = find_element(&document, parent_name, parent_key)
        .ok_or_else(|| format!("missing {parent_name}"))?;
    if let Some(child) = child_of(parent, child_name) {
        return patch_open_tag(xml, child, patch, allowed);
    }
    let prefix = qname_prefix(xml, parent)?;
    let mut attributes = String::new();
    for (key, value) in patch {
        if !allowed.contains(&key.as_str()) {
            return Err(format!("attribute {key} is not editable on {child_name}"));
        }
        if let Some(value) = attribute_value(value)? {
            attributes.push_str(&format!(" {key}=\"{}\"", escape_attribute(&value)));
        }
    }
    let child_xml = format!("<{prefix}{child_name}{attributes}/>");
    let parent_range = parent.range();
    let open_end = scan_open_tag_end(xml, parent_range.start)?;
    let open = &xml[parent_range.start..open_end];
    let mut output = xml.to_string();
    if open.trim_end().ends_with("/>") {
        let qname = format!("{prefix}{parent_name}");
        let mut expanded = open.to_string();
        let slash = expanded
            .rfind("/>")
            .ok_or_else(|| "invalid self-closing parent".to_string())?;
        expanded.replace_range(slash..slash + 2, ">");
        expanded.push_str(&child_xml);
        expanded.push_str(&format!("</{qname}>"));
        output.replace_range(parent_range, &expanded);
    } else {
        let insert_at = parent
            .children()
            .find(|node| node.is_element() && node.tag_name().name() == "extLst")
            .map(|node| node.range().start)
            .unwrap_or_else(|| {
                let close_len = format!("</{prefix}{parent_name}>").len();
                parent_range.end.saturating_sub(close_len)
            });
        output.insert_str(insert_at, &child_xml);
    }
    Ok(output)
}

const CONNECTION_ATTRS: &[&str] = &[
    "name",
    "description",
    "type",
    "refreshedVersion",
    "minRefreshableVersion",
    "refreshOnLoad",
    "background",
    "saveData",
    "deleted",
    "interval",
    "keepAlive",
    "reconnectionMethod",
    "onlyUseConnectionFile",
    "enableRefresh",
    "odcFile",
    "sourceFile",
    "singleSignOnId",
];
const DB_PR_ATTRS: &[&str] = &["connection", "command", "commandType", "serverCommand"];
const WEB_PR_ATTRS: &[&str] = &[
    "url",
    "post",
    "editPage",
    "htmlTables",
    "htmlFormat",
    "xml",
    "sourceData",
    "consecutive",
    "firstRow",
    "xl97",
    "textDates",
    "xl2000",
    "htmlTables",
];
const TEXT_PR_ATTRS: &[&str] = &[
    "sourceFile",
    "fileType",
    "codePage",
    "firstRow",
    "sourceFile",
    "delimited",
    "decimal",
    "thousands",
    "qualifier",
    "prompt",
    "tab",
    "space",
    "comma",
    "semicolon",
    "consecutive",
];
const QUERY_ATTRS: &[&str] = &[
    "name",
    "connectionId",
    "autoFormatId",
    "growShrinkType",
    "adjustColumnWidth",
    "preserveFormatting",
    "refreshOnLoad",
    "backgroundRefresh",
    "removeDataOnSave",
    "disableRefresh",
    "fillFormulas",
    "firstBackgroundRefresh",
    "headers",
    "rowNumbers",
    "intermediate",
    "applyNumberFormats",
    "applyBorderFormats",
    "applyFontFormats",
    "applyPatternFormats",
    "applyAlignmentFormats",
    "applyWidthHeightFormats",
];
const QUERY_REFRESH_ATTRS: &[&str] = &[
    "preserveSortFilterLayout",
    "fieldIdWrapped",
    "headersInLastRefresh",
    "minimumVersion",
    "nextId",
    "unboundColumnsLeft",
    "unboundColumnsRight",
];

fn patch_connection_xml(xml: &str, edit: &Map<String, Value>) -> Result<String, String> {
    let id_value = edit
        .get("id")
        .ok_or_else(|| "connection edit requires id".to_string())?;
    let id = match id_value {
        Value::String(value) => value.clone(),
        Value::Number(value) => value.to_string(),
        _ => return Err("connection id must be string/number".to_string()),
    };
    let mut output = xml.to_string();
    if let Some(attributes) = edit.get("attributes") {
        let attributes = attributes
            .as_object()
            .ok_or_else(|| "connection attributes must be an object".to_string())?;
        output = patch_element_attributes(
            &output,
            "connection",
            Some(("id", &id)),
            attributes,
            CONNECTION_ATTRS,
        )?;
    }
    let mut source = edit.get("source").cloned();
    if let Some(command) = edit.get("commandText") {
        let source_object = source.get_or_insert_with(|| Value::Object(Map::new()));
        if !source_object.is_object() {
            source = Some(json!({"connection":source_object.clone(), "command":command.clone()}));
        } else {
            source_object
                .as_object_mut()
                .unwrap()
                .insert("command".to_string(), command.clone());
        }
    }
    if let Some(source) = source {
        let source = if let Some(value) = source.as_str() {
            json!({"connection":value})
        } else {
            source
        };
        let source = source
            .as_object()
            .ok_or_else(|| "source must be string or object".to_string())?;
        let kind = source
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("database")
            .to_ascii_lowercase();
        let (child, allowed) = match kind.as_str() {
            "database" | "db" => ("dbPr", DB_PR_ATTRS),
            "web" => ("webPr", WEB_PR_ATTRS),
            "text" | "csv" => ("textPr", TEXT_PR_ATTRS),
            _ => return Err(format!("unsupported connection source kind {kind}")),
        };
        let mut attributes = source.clone();
        attributes.remove("kind");
        if let Some(command) = attributes.remove("commandText") {
            attributes.insert("command".to_string(), command);
        }
        if let Some(url) = attributes.remove("source") {
            attributes.insert(
                if child == "webPr" {
                    "url"
                } else if child == "textPr" {
                    "sourceFile"
                } else {
                    "connection"
                }
                .to_string(),
                url,
            );
        }
        output = patch_or_insert_child(
            &output,
            "connection",
            Some(("id", &id)),
            child,
            &attributes,
            allowed,
        )?;
    }
    Document::parse(&output)
        .map_err(|error| format!("connection edit produced invalid XML: {error}"))?;
    Ok(output)
}

fn patch_query_table_xml(xml: &str, edit: &Map<String, Value>) -> Result<String, String> {
    let mut output = xml.to_string();
    if let Some(attributes) = edit.get("attributes") {
        output = patch_element_attributes(
            &output,
            "queryTable",
            None,
            attributes
                .as_object()
                .ok_or_else(|| "queryTable attributes must be an object".to_string())?,
            QUERY_ATTRS,
        )?;
    }
    if let Some(refresh) = edit.get("refresh") {
        output = patch_or_insert_child(
            &output,
            "queryTable",
            None,
            "queryTableRefresh",
            refresh
                .as_object()
                .ok_or_else(|| "queryTable refresh must be an object".to_string())?,
            QUERY_REFRESH_ATTRS,
        )?;
    }
    Document::parse(&output)
        .map_err(|error| format!("queryTable edit produced invalid XML: {error}"))?;
    Ok(output)
}

fn patch_table_load_range(xml: &str, reference: &str) -> Result<String, String> {
    if !valid_a1_range(reference) {
        return Err(format!("invalid queryTable load range {reference}"));
    }
    let mut root_patch = Map::new();
    root_patch.insert("ref".to_string(), Value::String(reference.to_string()));
    let document = Document::parse(xml).map_err(|error| format!("invalid table XML: {error}"))?;
    let root = document.root_element();
    let mut output = patch_open_tag(xml, root, &root_patch, &["ref"])?;
    let document = Document::parse(&output).map_err(|error| error.to_string())?;
    if let Some(filter) = direct_child(document.root_element(), "autoFilter") {
        output = patch_open_tag(&output, filter, &root_patch, &["ref"])?;
    }
    Ok(output)
}

fn valid_a1_range(value: &str) -> bool {
    let mut parts = value.split(':');
    let valid_cell = |cell: &str| {
        let cell = cell
            .rsplit_once('!')
            .map(|(_, cell)| cell)
            .unwrap_or(cell)
            .replace('$', "");
        let split = cell
            .find(|character: char| character.is_ascii_digit())
            .unwrap_or(cell.len());
        split > 0
            && split < cell.len()
            && cell[..split]
                .chars()
                .all(|value| value.is_ascii_alphabetic())
            && cell[split..].parse::<u32>().is_ok_and(|row| row > 0)
    };
    let Some(first) = parts.next() else {
        return false;
    };
    let second = parts.next();
    parts.next().is_none() && valid_cell(first) && second.is_none_or(valid_cell)
}

fn edit_entries<'a>(patch: &'a Map<String, Value>, key: &str) -> Result<&'a [Value], String> {
    match patch.get(key) {
        None => Ok(&[]),
        Some(Value::Array(values)) => Ok(values),
        Some(_) => Err(format!("{key} must be an array")),
    }
}

fn verify_edit_precondition(
    parts: &BTreeMap<String, Vec<u8>>,
    part: &str,
    edit: &Map<String, Value>,
) -> Result<(), String> {
    let Some(expected) = edit.get("expectedSha256").and_then(Value::as_str) else {
        return Ok(());
    };
    let bytes = parts
        .get(part)
        .ok_or_else(|| format!("missing OPC part {part}"))?;
    let actual = sha256(bytes);
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(format!(
            "stale native-data edit for {part}: expected SHA-256 {expected}, found {actual}"
        ));
    }
    Ok(())
}

/// Atomically patches connection/query-table refresh metadata and linked table load ranges.
/// Opaque Mashup, custom-data, and VertiPaq parts are never decoded or rewritten.
pub(crate) fn apply_native_data_edit(
    parts: &mut BTreeMap<String, Vec<u8>>,
    patch: &Value,
) -> Result<Value, String> {
    let package = patch
        .as_object()
        .ok_or_else(|| "native data patch must be an object".to_string())?;
    let before = inspect_native_data(parts)?;
    let mut output = parts.clone();
    for value in edit_entries(package, "connectionEdits")? {
        let edit = value
            .as_object()
            .ok_or_else(|| "connection edit must be an object".to_string())?;
        let part = edit
            .get("part")
            .and_then(Value::as_str)
            .ok_or_else(|| "connection edit requires part".to_string())?;
        verify_edit_precondition(&output, part, edit)?;
        let xml = std::str::from_utf8(
            output
                .get(part)
                .ok_or_else(|| format!("missing connection part {part}"))?,
        )
        .map_err(|error| error.to_string())?;
        let updated = patch_connection_xml(xml, edit)?;
        output.insert(part.to_string(), updated.into_bytes());
    }
    let relationships = all_relationships(&output)?;
    for value in edit_entries(package, "queryTableEdits")? {
        let edit = value
            .as_object()
            .ok_or_else(|| "queryTable edit must be an object".to_string())?;
        let part = edit
            .get("part")
            .and_then(Value::as_str)
            .ok_or_else(|| "queryTable edit requires part".to_string())?;
        verify_edit_precondition(&output, part, edit)?;
        let xml = std::str::from_utf8(
            output
                .get(part)
                .ok_or_else(|| format!("missing queryTable part {part}"))?,
        )
        .map_err(|error| error.to_string())?;
        let updated = patch_query_table_xml(xml, edit)?;
        output.insert(part.to_string(), updated.into_bytes());
        if let Some(reference) = edit.get("loadRange").and_then(Value::as_str) {
            let linked = relationships
                .iter()
                .filter(|relationship| {
                    relationship.resolved_part.as_deref() == Some(part)
                        && relationship
                            .kind
                            .to_ascii_lowercase()
                            .contains("querytable")
                })
                .collect::<Vec<_>>();
            if linked.is_empty() {
                return Err(format!(
                    "queryTable {part} has no linked native table for loadRange"
                ));
            }
            for relation in linked {
                let table_xml = std::str::from_utf8(
                    output
                        .get(&relation.owner)
                        .ok_or_else(|| format!("missing linked table {}", relation.owner))?,
                )
                .map_err(|error| error.to_string())?;
                let updated_table = patch_table_load_range(table_xml, reference)?;
                output.insert(relation.owner.clone(), updated_table.into_bytes());
            }
        }
    }
    if edit_entries(package, "connectionEdits")?.is_empty()
        && edit_entries(package, "queryTableEdits")?.is_empty()
    {
        return Ok(before);
    }
    let model = inspect_native_data(&output)?;
    *parts = output;
    Ok(model)
}

#[derive(Clone, Debug, PartialEq)]
struct DataTable {
    columns: Vec<String>,
    rows: Vec<Map<String, Value>>,
}

const MAX_M_QUERY_BYTES: usize = 256 * 1024;
const MAX_M_INPUT_ROWS: usize = 100_000;
const MAX_M_COLUMNS: usize = 1_024;
const MAX_M_OUTPUT_ROWS: usize = 1_000_000;
const MAX_M_BINDINGS: usize = 512;

fn validate_table_limits(table: &DataTable, label: &str) -> Result<(), String> {
    if table.columns.len() > MAX_M_COLUMNS {
        return Err(format!(
            "{label} has {} columns; safe subset limit is {MAX_M_COLUMNS}",
            table.columns.len()
        ));
    }
    if table.rows.len() > MAX_M_INPUT_ROWS {
        return Err(format!(
            "{label} has {} rows; safe subset input limit is {MAX_M_INPUT_ROWS}",
            table.rows.len()
        ));
    }
    Ok(())
}

impl DataTable {
    fn from_rows(columns: Vec<String>, values: &[Value]) -> Result<Self, String> {
        let mut rows = Vec::new();
        for value in values {
            let row = match value {
                Value::Array(values) => columns
                    .iter()
                    .enumerate()
                    .map(|(index, column)| {
                        (
                            column.clone(),
                            values.get(index).cloned().unwrap_or(Value::Null),
                        )
                    })
                    .collect(),
                Value::Object(values) => columns
                    .iter()
                    .map(|column| {
                        (
                            column.clone(),
                            values.get(column).cloned().unwrap_or(Value::Null),
                        )
                    })
                    .collect(),
                _ => return Err("table rows must be arrays or objects".to_string()),
            };
            rows.push(row);
        }
        Ok(Self { columns, rows })
    }

    fn json_rows(&self) -> Vec<Value> {
        self.rows
            .iter()
            .map(|row| {
                Value::Array(
                    self.columns
                        .iter()
                        .map(|column| row.get(column).cloned().unwrap_or(Value::Null))
                        .collect(),
                )
            })
            .collect()
    }
}

fn infer_columns(rows: &[Value]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut columns = Vec::new();
    for row in rows {
        if let Value::Object(values) = row {
            for key in values.keys() {
                if seen.insert(key.clone()) {
                    columns.push(key.clone());
                }
            }
        }
    }
    columns
}

fn parse_csv(input: &str, delimiter: char) -> Result<DataTable, String> {
    let mut records = Vec::<Vec<String>>::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = input.chars().peekable();
    while let Some(character) = chars.next() {
        if quoted {
            if character == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                }
            } else {
                field.push(character);
            }
        } else {
            match character {
                '"' if field.is_empty() => quoted = true,
                value if value == delimiter => {
                    row.push(std::mem::take(&mut field));
                }
                '\n' => {
                    row.push(std::mem::take(&mut field));
                    records.push(std::mem::take(&mut row));
                }
                '\r' => {
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    row.push(std::mem::take(&mut field));
                    records.push(std::mem::take(&mut row));
                }
                value => field.push(value),
            }
        }
    }
    if quoted {
        return Err("unterminated quoted CSV field".to_string());
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        records.push(row);
    }
    if records.is_empty() {
        return Ok(DataTable {
            columns: Vec::new(),
            rows: Vec::new(),
        });
    }
    let columns = records.remove(0);
    if columns.iter().collect::<HashSet<_>>().len() != columns.len() {
        return Err("CSV header names must be unique".to_string());
    }
    let rows = records
        .into_iter()
        .map(|values| Value::Array(values.into_iter().map(Value::String).collect()))
        .collect::<Vec<_>>();
    DataTable::from_rows(columns, &rows)
}

fn table_from_input(value: &Value) -> Result<DataTable, String> {
    match value {
        Value::Array(rows) => {
            let columns = infer_columns(rows);
            if columns.is_empty() && rows.iter().all(Value::is_array) {
                return Err("array-row input requires explicit columns".to_string());
            }
            DataTable::from_rows(columns, rows)
        }
        Value::Object(object) => {
            for forbidden in [
                "path",
                "url",
                "uri",
                "connectionString",
                "credential",
                "credentials",
            ] {
                if object.contains_key(forbidden) {
                    return Err(format!(
                        "external input property {forbidden} is forbidden; supply in-memory data"
                    ));
                }
            }
            if let Some(csv) = object.get("csv").and_then(Value::as_str) {
                let delimiter = object
                    .get("delimiter")
                    .and_then(Value::as_str)
                    .and_then(|value| value.chars().next())
                    .unwrap_or(',');
                return parse_csv(csv, delimiter);
            }
            if let Some(json_value) = object.get("json") {
                let parsed;
                let value = if let Some(text) = json_value.as_str() {
                    parsed = serde_json::from_str::<Value>(text)
                        .map_err(|error| format!("invalid JSON input: {error}"))?;
                    &parsed
                } else {
                    json_value
                };
                return table_from_input(value);
            }
            if let Some(rows) = object.get("rows").and_then(Value::as_array) {
                let columns = object
                    .get("columns")
                    .and_then(Value::as_array)
                    .map(|values| {
                        values
                            .iter()
                            .map(|value| {
                                value
                                    .as_str()
                                    .map(str::to_string)
                                    .ok_or_else(|| "column names must be strings".to_string())
                            })
                            .collect::<Result<Vec<_>, _>>()
                    })
                    .transpose()?
                    .unwrap_or_else(|| infer_columns(rows));
                return DataTable::from_rows(columns, rows);
            }
            // A single object is a one-row table.
            let columns = object.keys().cloned().collect::<Vec<_>>();
            DataTable::from_rows(columns, &[Value::Object(object.clone())])
        }
        _ => Err("input must be a table, CSV, or JSON object/array".to_string()),
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Identifier(String),
    String(String),
    Number(f64),
    Symbol(char),
    Operator(String),
}

fn tokenize_m(input: &str) -> Result<Vec<Token>, String> {
    let chars = input.chars().collect::<Vec<_>>();
    let mut cursor = 0usize;
    let mut tokens = Vec::new();
    while cursor < chars.len() {
        if chars[cursor].is_whitespace() {
            cursor += 1;
            continue;
        }
        if chars[cursor] == '/' && chars.get(cursor + 1) == Some(&'/') {
            cursor += 2;
            while cursor < chars.len() && chars[cursor] != '\n' {
                cursor += 1;
            }
            continue;
        }
        if chars[cursor] == '/' && chars.get(cursor + 1) == Some(&'*') {
            cursor += 2;
            while cursor + 1 < chars.len() && !(chars[cursor] == '*' && chars[cursor + 1] == '/') {
                cursor += 1;
            }
            if cursor + 1 >= chars.len() {
                return Err("unterminated M block comment".to_string());
            }
            cursor += 2;
            continue;
        }
        if chars[cursor] == '#' && chars.get(cursor + 1) == Some(&'"') {
            cursor += 2;
            let mut value = String::new();
            loop {
                let Some(character) = chars.get(cursor).copied() else {
                    return Err("unterminated quoted M identifier".to_string());
                };
                cursor += 1;
                if character == '"' {
                    if chars.get(cursor) == Some(&'"') {
                        cursor += 1;
                        value.push('"');
                        continue;
                    }
                    break;
                }
                value.push(character);
            }
            tokens.push(Token::Identifier(value));
            continue;
        }
        if chars[cursor] == '"' {
            cursor += 1;
            let mut value = String::new();
            loop {
                let Some(character) = chars.get(cursor).copied() else {
                    return Err("unterminated M string".to_string());
                };
                cursor += 1;
                if character == '"' {
                    if chars.get(cursor) == Some(&'"') {
                        cursor += 1;
                        value.push('"');
                        continue;
                    }
                    break;
                }
                if character == '#' && chars.get(cursor) == Some(&'(') {
                    // Common M escapes: #(lf), #(cr), #(tab), #(quote).
                    let escape_start = cursor + 1;
                    if let Some(end) = chars[escape_start..].iter().position(|value| *value == ')')
                    {
                        let escape = chars[escape_start..escape_start + end]
                            .iter()
                            .collect::<String>()
                            .to_ascii_lowercase();
                        cursor = escape_start + end + 1;
                        value.push(match escape.as_str() {
                            "lf" => '\n',
                            "cr" => '\r',
                            "tab" => '\t',
                            "quote" => '"',
                            _ => return Err(format!("unsupported M string escape #({escape})")),
                        });
                        continue;
                    }
                }
                value.push(character);
            }
            tokens.push(Token::String(value));
            continue;
        }
        if chars[cursor].is_ascii_digit()
            || (chars[cursor] == '.' && chars.get(cursor + 1).is_some_and(char::is_ascii_digit))
        {
            let start = cursor;
            cursor += 1;
            while cursor < chars.len()
                && (chars[cursor].is_ascii_digit()
                    || matches!(chars[cursor], '.' | 'e' | 'E' | '+' | '-')
                        && matches!(chars.get(cursor.wrapping_sub(1)), Some('e' | 'E')))
            {
                cursor += 1;
            }
            let text = chars[start..cursor].iter().collect::<String>();
            tokens.push(Token::Number(
                text.parse()
                    .map_err(|_| format!("invalid M number {text}"))?,
            ));
            continue;
        }
        if chars[cursor].is_alphabetic() || matches!(chars[cursor], '_' | '#') {
            let start = cursor;
            cursor += 1;
            while cursor < chars.len()
                && (chars[cursor].is_alphanumeric() || matches!(chars[cursor], '_' | '.' | '#'))
            {
                cursor += 1;
            }
            tokens.push(Token::Identifier(chars[start..cursor].iter().collect()));
            continue;
        }
        if let Some(pair) = chars
            .get(cursor..cursor + 2)
            .map(|values| values.iter().collect::<String>())
            .filter(|pair| matches!(pair.as_str(), "<>" | "<=" | ">=" | "=>"))
        {
            tokens.push(Token::Operator(pair));
            cursor += 2;
            continue;
        }
        let character = chars[cursor];
        cursor += 1;
        if "(){}[],".contains(character) {
            tokens.push(Token::Symbol(character));
        } else if "=<>+-*/&".contains(character) {
            tokens.push(Token::Operator(character.to_string()));
        } else {
            return Err(format!("unsupported M token {character}"));
        }
    }
    Ok(tokens)
}

#[derive(Clone, Debug)]
enum Expr {
    Literal(Value),
    Identifier(String),
    Field(String),
    List(Vec<Expr>),
    Each(Box<Expr>),
    Call(String, Vec<Expr>),
    Unary(String, Box<Expr>),
    Binary(String, Box<Expr>, Box<Expr>),
}

#[derive(Clone, Debug)]
struct MProgram {
    bindings: Vec<(String, Expr)>,
    output: Expr,
}

struct MParser {
    tokens: Vec<Token>,
    cursor: usize,
}

impl MParser {
    fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, cursor: 0 }
    }
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.cursor)
    }
    fn next(&mut self) -> Option<Token> {
        let value = self.tokens.get(self.cursor).cloned();
        self.cursor += usize::from(value.is_some());
        value
    }
    fn identifier_is(&self, value: &str) -> bool {
        matches!(self.peek(), Some(Token::Identifier(current)) if current.eq_ignore_ascii_case(value))
    }
    fn take_identifier(&mut self) -> Result<String, String> {
        match self.next() {
            Some(Token::Identifier(value)) => Ok(value),
            other => Err(format!("expected identifier, got {other:?}")),
        }
    }
    fn expect_symbol(&mut self, expected: char) -> Result<(), String> {
        match self.next() {
            Some(Token::Symbol(value)) if value == expected => Ok(()),
            other => Err(format!("expected '{expected}', got {other:?}")),
        }
    }
    fn expect_operator(&mut self, expected: &str) -> Result<(), String> {
        match self.next() {
            Some(Token::Operator(value)) if value == expected => Ok(()),
            other => Err(format!("expected '{expected}', got {other:?}")),
        }
    }

    fn parse_program(&mut self) -> Result<MProgram, String> {
        let mut bindings = Vec::new();
        if self.identifier_is("let") {
            self.next();
            loop {
                if self.identifier_is("in") {
                    self.next();
                    break;
                }
                let name = self.take_identifier()?;
                self.expect_operator("=")?;
                let expression = self.parse_expression(0)?;
                bindings.push((name, expression));
                if bindings.len() > MAX_M_BINDINGS {
                    return Err(format!(
                        "M query exceeds the safe limit of {MAX_M_BINDINGS} let bindings"
                    ));
                }
                match self.peek() {
                    Some(Token::Symbol(',')) => {
                        self.next();
                    }
                    Some(Token::Identifier(value)) if value.eq_ignore_ascii_case("in") => {
                        self.next();
                        break;
                    }
                    other => return Err(format!("expected ',' or 'in', got {other:?}")),
                }
            }
        }
        let output = self.parse_expression(0)?;
        if self.peek().is_some() {
            return Err(format!("unexpected trailing M token {:?}", self.peek()));
        }
        Ok(MProgram { bindings, output })
    }

    fn precedence(token: &Token) -> Option<(u8, String)> {
        match token {
            Token::Identifier(value) if value.eq_ignore_ascii_case("or") => {
                Some((1, "or".to_string()))
            }
            Token::Identifier(value) if value.eq_ignore_ascii_case("and") => {
                Some((2, "and".to_string()))
            }
            Token::Operator(value)
                if matches!(value.as_str(), "=" | "<>" | "<" | ">" | "<=" | ">=") =>
            {
                Some((3, value.clone()))
            }
            Token::Operator(value) if value == "&" => Some((4, value.clone())),
            Token::Operator(value) if matches!(value.as_str(), "+" | "-") => {
                Some((5, value.clone()))
            }
            Token::Operator(value) if matches!(value.as_str(), "*" | "/") => {
                Some((6, value.clone()))
            }
            _ => None,
        }
    }

    fn parse_expression(&mut self, minimum: u8) -> Result<Expr, String> {
        let mut left = self.parse_unary()?;
        loop {
            let Some((precedence, operator)) = self.peek().and_then(Self::precedence) else {
                break;
            };
            if precedence < minimum {
                break;
            }
            self.next();
            let right = self.parse_expression(precedence + 1)?;
            left = Expr::Binary(operator, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr, String> {
        if self.identifier_is("not") {
            self.next();
            return Ok(Expr::Unary(
                "not".to_string(),
                Box::new(self.parse_unary()?),
            ));
        }
        if matches!(self.peek(), Some(Token::Operator(value)) if matches!(value.as_str(), "+" | "-"))
        {
            let Token::Operator(operator) = self.next().unwrap() else {
                unreachable!()
            };
            return Ok(Expr::Unary(operator, Box::new(self.parse_unary()?)));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Expr, String> {
        match self.next() {
            Some(Token::String(value)) => Ok(Expr::Literal(Value::String(value))),
            Some(Token::Number(value)) => Ok(Expr::Literal(
                Number::from_f64(value)
                    .map(Value::Number)
                    .ok_or_else(|| "non-finite M number".to_string())?,
            )),
            Some(Token::Identifier(value)) if value.eq_ignore_ascii_case("true") => {
                Ok(Expr::Literal(Value::Bool(true)))
            }
            Some(Token::Identifier(value)) if value.eq_ignore_ascii_case("false") => {
                Ok(Expr::Literal(Value::Bool(false)))
            }
            Some(Token::Identifier(value)) if value.eq_ignore_ascii_case("null") => {
                Ok(Expr::Literal(Value::Null))
            }
            Some(Token::Identifier(value)) if value.eq_ignore_ascii_case("each") => {
                Ok(Expr::Each(Box::new(self.parse_expression(0)?)))
            }
            Some(Token::Identifier(value)) if value.eq_ignore_ascii_case("type") => {
                let name = self.take_identifier()?;
                Ok(Expr::Identifier(format!("type {name}")))
            }
            Some(Token::Identifier(value)) => {
                if matches!(self.peek(), Some(Token::Symbol('('))) {
                    self.next();
                    let mut arguments = Vec::new();
                    if !matches!(self.peek(), Some(Token::Symbol(')'))) {
                        loop {
                            arguments.push(self.parse_expression(0)?);
                            if matches!(self.peek(), Some(Token::Symbol(','))) {
                                self.next();
                                continue;
                            }
                            break;
                        }
                    }
                    self.expect_symbol(')')?;
                    Ok(Expr::Call(value, arguments))
                } else {
                    Ok(Expr::Identifier(value))
                }
            }
            Some(Token::Symbol('(')) => {
                let expression = self.parse_expression(0)?;
                self.expect_symbol(')')?;
                Ok(expression)
            }
            Some(Token::Symbol('{')) => {
                let mut values = Vec::new();
                if !matches!(self.peek(), Some(Token::Symbol('}'))) {
                    loop {
                        values.push(self.parse_expression(0)?);
                        if matches!(self.peek(), Some(Token::Symbol(','))) {
                            self.next();
                            continue;
                        }
                        break;
                    }
                }
                self.expect_symbol('}')?;
                Ok(Expr::List(values))
            }
            Some(Token::Symbol('[')) => {
                let name = match self.next() {
                    Some(Token::Identifier(value)) | Some(Token::String(value)) => value,
                    other => return Err(format!("expected field name, got {other:?}")),
                };
                self.expect_symbol(']')?;
                Ok(Expr::Field(name))
            }
            other => Err(format!("expected M expression, got {other:?}")),
        }
    }
}

#[derive(Clone, Debug)]
enum RuntimeValue {
    Json(Value),
    Table(DataTable),
    List(Vec<RuntimeValue>),
    Lambda(Expr),
}

#[derive(Clone, Copy)]
enum RowContext<'a> {
    Row(&'a Map<String, Value>),
    Group(&'a DataTable),
}

fn runtime_json(value: RuntimeValue) -> Result<Value, String> {
    match value {
        RuntimeValue::Json(value) => Ok(value),
        RuntimeValue::List(values) => Ok(Value::Array(
            values
                .into_iter()
                .map(runtime_json)
                .collect::<Result<Vec<_>, _>>()?,
        )),
        RuntimeValue::Table(table) => {
            Ok(json!({"columns":table.columns, "rows":table.json_rows()}))
        }
        RuntimeValue::Lambda(_) => Err("lambda cannot be converted to a scalar".to_string()),
    }
}

fn expect_table(value: RuntimeValue) -> Result<DataTable, String> {
    match value {
        RuntimeValue::Table(table) => Ok(table),
        _ => Err("expected a table".to_string()),
    }
}
fn expect_list(value: RuntimeValue) -> Result<Vec<RuntimeValue>, String> {
    match value {
        RuntimeValue::List(values) => Ok(values),
        RuntimeValue::Json(Value::Array(values)) => {
            Ok(values.into_iter().map(RuntimeValue::Json).collect())
        }
        _ => Err("expected a list".to_string()),
    }
}
fn expect_string(value: RuntimeValue) -> Result<String, String> {
    match runtime_json(value)? {
        Value::String(value) => Ok(value),
        other => Err(format!("expected text, got {other}")),
    }
}
fn expect_usize(value: RuntimeValue) -> Result<usize, String> {
    match runtime_json(value)? {
        Value::Number(value) => value
            .as_u64()
            .map(|value| value as usize)
            .ok_or_else(|| "expected non-negative integer".to_string()),
        other => Err(format!("expected integer, got {other}")),
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}

fn value_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(value) => value.as_f64(),
        Value::String(value) => value.trim().parse().ok(),
        Value::Bool(value) => Some(if *value { 1.0 } else { 0.0 }),
        _ => None,
    }
}

fn scalar_compare(left: &Value, right: &Value) -> Ordering {
    match (value_number(left), value_number(right)) {
        (Some(left), Some(right)) => left.partial_cmp(&right).unwrap_or(Ordering::Equal),
        _ => match (left, right) {
            (Value::Null, Value::Null) => Ordering::Equal,
            (Value::Null, _) => Ordering::Less,
            (_, Value::Null) => Ordering::Greater,
            (Value::String(left), Value::String(right)) => left.cmp(right),
            (Value::Bool(left), Value::Bool(right)) => left.cmp(right),
            _ => left.to_string().cmp(&right.to_string()),
        },
    }
}

fn json_number(value: f64) -> Value {
    Number::from_f64(value)
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

fn binary_value(operator: &str, left: Value, right: Value) -> Result<Value, String> {
    Ok(match operator {
        "and" => Value::Bool(truthy(&left) && truthy(&right)),
        "or" => Value::Bool(truthy(&left) || truthy(&right)),
        "=" => Value::Bool(scalar_compare(&left, &right) == Ordering::Equal),
        "<>" => Value::Bool(scalar_compare(&left, &right) != Ordering::Equal),
        "<" => Value::Bool(scalar_compare(&left, &right) == Ordering::Less),
        ">" => Value::Bool(scalar_compare(&left, &right) == Ordering::Greater),
        "<=" => Value::Bool(scalar_compare(&left, &right) != Ordering::Greater),
        ">=" => Value::Bool(scalar_compare(&left, &right) != Ordering::Less),
        "&" => Value::String(format!(
            "{}{}",
            if left.is_string() {
                left.as_str().unwrap().to_string()
            } else {
                left.to_string()
            },
            if right.is_string() {
                right.as_str().unwrap().to_string()
            } else {
                right.to_string()
            }
        )),
        "+" | "-" | "*" | "/" => {
            let left = value_number(&left)
                .ok_or_else(|| format!("left operand of {operator} is not numeric"))?;
            let right = value_number(&right)
                .ok_or_else(|| format!("right operand of {operator} is not numeric"))?;
            if operator == "/" && right == 0.0 {
                return Err("division by zero".to_string());
            }
            json_number(match operator {
                "+" => left + right,
                "-" => left - right,
                "*" => left * right,
                "/" => left / right,
                _ => unreachable!(),
            })
        }
        _ => return Err(format!("unsupported operator {operator}")),
    })
}

fn lookup_env<'a>(
    environment: &'a HashMap<String, RuntimeValue>,
    name: &str,
) -> Option<&'a RuntimeValue> {
    environment.get(name).or_else(|| {
        environment
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value)
    })
}

fn field_value(context: Option<RowContext<'_>>, name: &str) -> Result<RuntimeValue, String> {
    match context {
        Some(RowContext::Row(row)) => Ok(RuntimeValue::Json(
            row.get(name).cloned().unwrap_or(Value::Null),
        )),
        Some(RowContext::Group(table)) => Ok(RuntimeValue::List(
            table
                .rows
                .iter()
                .map(|row| RuntimeValue::Json(row.get(name).cloned().unwrap_or(Value::Null)))
                .collect(),
        )),
        None => Err(format!("field [{name}] used outside row/group context")),
    }
}

fn eval_expr(
    expression: &Expr,
    environment: &HashMap<String, RuntimeValue>,
    context: Option<RowContext<'_>>,
    diagnostics: &mut Vec<Value>,
) -> Result<RuntimeValue, String> {
    match expression {
        Expr::Literal(value) => Ok(RuntimeValue::Json(value.clone())),
        Expr::Identifier(name) if name == "_" => match context {
            Some(RowContext::Row(row)) => Ok(RuntimeValue::Json(Value::Object(row.clone()))),
            Some(RowContext::Group(table)) => Ok(RuntimeValue::Table(table.clone())),
            None => Err("'_' used outside lambda".to_string()),
        },
        Expr::Identifier(name)
            if name.starts_with("type ")
                || name.ends_with(".Type")
                || name.starts_with("Order.")
                || name.starts_with("JoinKind.") =>
        {
            Ok(RuntimeValue::Json(Value::String(name.clone())))
        }
        Expr::Identifier(name) => lookup_env(environment, name)
            .cloned()
            .ok_or_else(|| format!("unknown M identifier {name}")),
        Expr::Field(name) => field_value(context, name),
        Expr::List(values) => Ok(RuntimeValue::List(
            values
                .iter()
                .map(|value| eval_expr(value, environment, context, diagnostics))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        Expr::Each(value) => Ok(RuntimeValue::Lambda((**value).clone())),
        Expr::Unary(operator, value) => {
            let value = runtime_json(eval_expr(value, environment, context, diagnostics)?)?;
            Ok(RuntimeValue::Json(match operator.as_str() {
                "not" => Value::Bool(!truthy(&value)),
                "+" => json_number(
                    value_number(&value).ok_or_else(|| "unary + expects number".to_string())?,
                ),
                "-" => json_number(
                    -value_number(&value).ok_or_else(|| "unary - expects number".to_string())?,
                ),
                _ => return Err(format!("unsupported unary operator {operator}")),
            }))
        }
        Expr::Binary(operator, left, right) => {
            let left = runtime_json(eval_expr(left, environment, context, diagnostics)?)?;
            // Preserve M-like short circuiting for row predicates.
            if operator == "and" && !truthy(&left) {
                return Ok(RuntimeValue::Json(Value::Bool(false)));
            }
            if operator == "or" && truthy(&left) {
                return Ok(RuntimeValue::Json(Value::Bool(true)));
            }
            let right = runtime_json(eval_expr(right, environment, context, diagnostics)?)?;
            Ok(RuntimeValue::Json(binary_value(operator, left, right)?))
        }
        Expr::Call(name, arguments) => {
            eval_call(name, arguments, environment, context, diagnostics)
        }
    }
}

fn string_list(value: RuntimeValue) -> Result<Vec<String>, String> {
    expect_list(value)?.into_iter().map(expect_string).collect()
}

fn lambda_expr(value: RuntimeValue) -> Result<Expr, String> {
    match value {
        RuntimeValue::Lambda(expression) => Ok(expression),
        _ => Err("expected an 'each' lambda".to_string()),
    }
}

fn diagnostic_step(
    diagnostics: &mut Vec<Value>,
    function: &str,
    input_rows: usize,
    output_rows: usize,
) {
    diagnostics.push(json!({"severity":"info", "code":"M_STEP", "function":function, "inputRows":input_rows, "outputRows":output_rows}));
}

fn transform_value(value: Value, type_name: &str) -> Result<Value, String> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    let normalized = type_name.to_ascii_lowercase();
    if normalized.contains("text") {
        return Ok(Value::String(match value {
            Value::String(value) => value,
            other => other.to_string(),
        }));
    }
    if normalized.contains("logical") || normalized.contains("bool") {
        return Ok(Value::Bool(match value {
            Value::Bool(value) => value,
            Value::String(value) if value.eq_ignore_ascii_case("true") => true,
            Value::String(value) if value.eq_ignore_ascii_case("false") => false,
            other => value_number(&other).is_some_and(|value| value != 0.0),
        }));
    }
    if normalized.contains("int") || normalized.contains("whole") {
        let number =
            value_number(&value).ok_or_else(|| format!("cannot convert {value} to integer"))?;
        return Ok(Value::Number(Number::from(number.trunc() as i64)));
    }
    if normalized.contains("number")
        || normalized.contains("decimal")
        || normalized.contains("double")
        || normalized.contains("currency")
    {
        return Ok(json_number(
            value_number(&value).ok_or_else(|| format!("cannot convert {value} to number"))?,
        ));
    }
    if normalized.contains("date") || normalized.contains("time") {
        return Ok(Value::String(match value {
            Value::String(value) => value,
            other => other.to_string(),
        }));
    }
    Err(format!("unsupported target type {type_name}"))
}

fn list_aggregate(name: &str, values: Vec<RuntimeValue>) -> Result<RuntimeValue, String> {
    let values = values
        .into_iter()
        .map(runtime_json)
        .collect::<Result<Vec<_>, _>>()?;
    let non_null = values
        .iter()
        .filter(|value| !value.is_null())
        .cloned()
        .collect::<Vec<_>>();
    match name {
        "list.count" => Ok(RuntimeValue::Json(Value::Number(Number::from(
            values.len(),
        )))),
        "list.sum" | "list.average" => {
            let numbers = non_null
                .iter()
                .map(|value| {
                    value_number(value)
                        .ok_or_else(|| format!("aggregate value {value} is not numeric"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            if name == "list.average" && numbers.is_empty() {
                return Ok(RuntimeValue::Json(Value::Null));
            }
            let sum = numbers.iter().sum::<f64>();
            Ok(RuntimeValue::Json(json_number(if name == "list.average" {
                sum / numbers.len() as f64
            } else {
                sum
            })))
        }
        "list.min" | "list.max" => {
            let value = non_null
                .into_iter()
                .reduce(|left, right| {
                    let order = scalar_compare(&left, &right);
                    if (name == "list.min" && order == Ordering::Greater)
                        || (name == "list.max" && order == Ordering::Less)
                    {
                        right
                    } else {
                        left
                    }
                })
                .unwrap_or(Value::Null);
            Ok(RuntimeValue::Json(value))
        }
        _ => Err(format!("unsupported list aggregate {name}")),
    }
}

fn row_key(row: &Map<String, Value>, columns: &[String]) -> String {
    Value::Array(
        columns
            .iter()
            .map(|column| row.get(column).cloned().unwrap_or(Value::Null))
            .collect(),
    )
    .to_string()
}

fn join_tables(
    left: DataTable,
    left_keys: &[String],
    right: DataTable,
    right_keys: &[String],
    kind: &str,
) -> Result<DataTable, String> {
    if left_keys.len() != right_keys.len() || left_keys.is_empty() {
        return Err("join key lists must be non-empty and have equal length".to_string());
    }
    for key in left_keys {
        if !left.columns.contains(key) {
            return Err(format!("left join column {key} does not exist"));
        }
    }
    for key in right_keys {
        if !right.columns.contains(key) {
            return Err(format!("right join column {key} does not exist"));
        }
    }
    let mut right_names = Vec::new();
    let mut columns = left.columns.clone();
    for column in &right.columns {
        let mut name = column.clone();
        if columns.contains(&name) {
            let mut suffix = 1usize;
            while columns.contains(&format!("{column}.{suffix}")) {
                suffix += 1;
            }
            name = format!("{column}.{suffix}");
        }
        columns.push(name.clone());
        right_names.push(name);
    }
    let mut index: HashMap<String, Vec<usize>> = HashMap::new();
    for (row_index, row) in right.rows.iter().enumerate() {
        index
            .entry(row_key(row, right_keys))
            .or_default()
            .push(row_index);
    }
    let mut matched_right = vec![false; right.rows.len()];
    let mut rows = Vec::new();
    let lower = kind.to_ascii_lowercase();
    for left_row in &left.rows {
        let matches = index
            .get(&row_key(left_row, left_keys))
            .cloned()
            .unwrap_or_default();
        if lower.ends_with("leftanti") {
            if matches.is_empty() {
                rows.push(left_row.clone());
            }
            continue;
        }
        if matches.is_empty() {
            if lower.ends_with("leftouter") || lower.ends_with("fullouter") {
                let mut row = left_row.clone();
                for name in &right_names {
                    row.insert(name.clone(), Value::Null);
                }
                rows.push(row);
            }
            continue;
        }
        for right_index in matches {
            matched_right[right_index] = true;
            let mut row = left_row.clone();
            for (index, column) in right.columns.iter().enumerate() {
                row.insert(
                    right_names[index].clone(),
                    right.rows[right_index]
                        .get(column)
                        .cloned()
                        .unwrap_or(Value::Null),
                );
            }
            rows.push(row);
            if rows.len() > MAX_M_OUTPUT_ROWS {
                return Err(format!(
                    "join exceeds the safe output limit of {MAX_M_OUTPUT_ROWS} rows"
                ));
            }
        }
    }
    if lower.ends_with("rightouter") || lower.ends_with("fullouter") || lower.ends_with("rightanti")
    {
        for (_index, right_row) in right
            .rows
            .iter()
            .enumerate()
            .filter(|(index, _)| !matched_right[*index])
        {
            let mut row = Map::new();
            for column in &left.columns {
                row.insert(column.clone(), Value::Null);
            }
            for (column_index, column) in right.columns.iter().enumerate() {
                row.insert(
                    right_names[column_index].clone(),
                    right_row.get(column).cloned().unwrap_or(Value::Null),
                );
            }
            rows.push(row);
        }
    }
    if lower.ends_with("leftanti") {
        columns = left.columns;
    }
    if lower.ends_with("rightanti") {
        columns = right_names.clone();
        rows = rows
            .into_iter()
            .map(|row| {
                right_names
                    .iter()
                    .map(|column| {
                        (
                            column.clone(),
                            row.get(column).cloned().unwrap_or(Value::Null),
                        )
                    })
                    .collect()
            })
            .collect();
    }
    Ok(DataTable { columns, rows })
}

fn eval_call(
    name: &str,
    arguments: &[Expr],
    environment: &HashMap<String, RuntimeValue>,
    context: Option<RowContext<'_>>,
    diagnostics: &mut Vec<Value>,
) -> Result<RuntimeValue, String> {
    let function = name.to_ascii_lowercase();
    match function.as_str() {
        "#table" | "table.fromrows" => {
            if arguments.len() < 2 {
                return Err(format!("{name} requires columns and rows"));
            }
            let first = eval_expr(&arguments[0], environment, context, diagnostics)?;
            let second = eval_expr(&arguments[1], environment, context, diagnostics)?;
            let (columns, rows) = if function == "#table" {
                (string_list(first)?, expect_list(second)?)
            } else {
                (string_list(second)?, expect_list(first)?)
            };
            let rows = rows
                .into_iter()
                .map(runtime_json)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(RuntimeValue::Table(DataTable::from_rows(columns, &rows)?))
        }
        "table.selectrows" => {
            if arguments.len() != 2 {
                return Err("Table.SelectRows requires table and predicate".to_string());
            }
            let table = expect_table(eval_expr(&arguments[0], environment, context, diagnostics)?)?;
            let predicate =
                lambda_expr(eval_expr(&arguments[1], environment, context, diagnostics)?)?;
            let mut rows = Vec::new();
            for row in &table.rows {
                let value = runtime_json(eval_expr(
                    &predicate,
                    environment,
                    Some(RowContext::Row(row)),
                    diagnostics,
                )?)?;
                if truthy(&value) {
                    rows.push(row.clone());
                }
            }
            diagnostic_step(diagnostics, name, table.rows.len(), rows.len());
            Ok(RuntimeValue::Table(DataTable {
                columns: table.columns,
                rows,
            }))
        }
        "table.selectcolumns" => {
            if arguments.len() < 2 {
                return Err("Table.SelectColumns requires table and columns".to_string());
            }
            let table = expect_table(eval_expr(&arguments[0], environment, context, diagnostics)?)?;
            let columns =
                string_list(eval_expr(&arguments[1], environment, context, diagnostics)?)?;
            for column in &columns {
                if !table.columns.contains(column) {
                    return Err(format!("column {column} does not exist"));
                }
            }
            let rows = table
                .rows
                .iter()
                .map(|row| {
                    columns
                        .iter()
                        .map(|column| {
                            (
                                column.clone(),
                                row.get(column).cloned().unwrap_or(Value::Null),
                            )
                        })
                        .collect()
                })
                .collect();
            diagnostic_step(diagnostics, name, table.rows.len(), table.rows.len());
            Ok(RuntimeValue::Table(DataTable { columns, rows }))
        }
        "table.renamecolumns" => {
            if arguments.len() < 2 {
                return Err("Table.RenameColumns requires table and rename pairs".to_string());
            }
            let mut table =
                expect_table(eval_expr(&arguments[0], environment, context, diagnostics)?)?;
            let pairs = expect_list(eval_expr(&arguments[1], environment, context, diagnostics)?)?;
            for pair in pairs {
                let values = expect_list(pair)?;
                if values.len() < 2 {
                    return Err("rename pair requires old and new name".to_string());
                }
                let old = expect_string(values[0].clone())?;
                let new = expect_string(values[1].clone())?;
                if old != new && table.columns.contains(&new) {
                    return Err(format!("column {new} already exists"));
                }
                let index = table
                    .columns
                    .iter()
                    .position(|column| column == &old)
                    .ok_or_else(|| format!("column {old} does not exist"))?;
                table.columns[index] = new.clone();
                for row in &mut table.rows {
                    let value = row.remove(&old).unwrap_or(Value::Null);
                    row.insert(new.clone(), value);
                }
            }
            diagnostic_step(diagnostics, name, table.rows.len(), table.rows.len());
            Ok(RuntimeValue::Table(table))
        }
        "table.addcolumn" => {
            if arguments.len() < 3 {
                return Err("Table.AddColumn requires table, name, and generator".to_string());
            }
            let mut table =
                expect_table(eval_expr(&arguments[0], environment, context, diagnostics)?)?;
            let column =
                expect_string(eval_expr(&arguments[1], environment, context, diagnostics)?)?;
            if table.columns.contains(&column) {
                return Err(format!("column {column} already exists"));
            }
            let generator =
                lambda_expr(eval_expr(&arguments[2], environment, context, diagnostics)?)?;
            let type_name = arguments
                .get(3)
                .map(|expression| {
                    eval_expr(expression, environment, context, diagnostics).and_then(expect_string)
                })
                .transpose()?;
            for row in &mut table.rows {
                let mut value = runtime_json(eval_expr(
                    &generator,
                    environment,
                    Some(RowContext::Row(row)),
                    diagnostics,
                )?)?;
                if let Some(type_name) = &type_name {
                    value = transform_value(value, type_name)?;
                }
                row.insert(column.clone(), value);
            }
            table.columns.push(column);
            diagnostic_step(diagnostics, name, table.rows.len(), table.rows.len());
            Ok(RuntimeValue::Table(table))
        }
        "table.transformcolumntypes" => {
            if arguments.len() < 2 {
                return Err("Table.TransformColumnTypes requires table and transforms".to_string());
            }
            let mut table =
                expect_table(eval_expr(&arguments[0], environment, context, diagnostics)?)?;
            let transforms =
                expect_list(eval_expr(&arguments[1], environment, context, diagnostics)?)?;
            for transform in transforms {
                let values = expect_list(transform)?;
                if values.len() < 2 {
                    return Err("type transform requires column and type".to_string());
                }
                let column = expect_string(values[0].clone())?;
                let type_name = expect_string(values[1].clone())?;
                if !table.columns.contains(&column) {
                    return Err(format!("column {column} does not exist"));
                }
                for row in &mut table.rows {
                    let value = row.remove(&column).unwrap_or(Value::Null);
                    row.insert(column.clone(), transform_value(value, &type_name)?);
                }
            }
            diagnostic_step(diagnostics, name, table.rows.len(), table.rows.len());
            Ok(RuntimeValue::Table(table))
        }
        "table.sort" => {
            if arguments.len() < 2 {
                return Err("Table.Sort requires table and sort descriptors".to_string());
            }
            let mut table =
                expect_table(eval_expr(&arguments[0], environment, context, diagnostics)?)?;
            let descriptors =
                expect_list(eval_expr(&arguments[1], environment, context, diagnostics)?)?;
            let mut sort = Vec::new();
            for descriptor in descriptors {
                match descriptor {
                    RuntimeValue::Json(Value::String(column)) => sort.push((column, false)),
                    value => {
                        let pair = expect_list(value)?;
                        let column = expect_string(
                            pair.first()
                                .cloned()
                                .ok_or_else(|| "empty sort descriptor".to_string())?,
                        )?;
                        let direction = pair
                            .get(1)
                            .cloned()
                            .map(expect_string)
                            .transpose()?
                            .unwrap_or_else(|| "Order.Ascending".to_string());
                        sort.push((
                            column,
                            direction.to_ascii_lowercase().ends_with("descending"),
                        ));
                    }
                }
            }
            for (column, _) in &sort {
                if !table.columns.contains(column) {
                    return Err(format!("sort column {column} does not exist"));
                }
            }
            table.rows.sort_by(|left, right| {
                for (column, descending) in &sort {
                    let ordering = scalar_compare(
                        left.get(column).unwrap_or(&Value::Null),
                        right.get(column).unwrap_or(&Value::Null),
                    );
                    if ordering != Ordering::Equal {
                        return if *descending {
                            ordering.reverse()
                        } else {
                            ordering
                        };
                    }
                }
                Ordering::Equal
            });
            diagnostic_step(diagnostics, name, table.rows.len(), table.rows.len());
            Ok(RuntimeValue::Table(table))
        }
        "table.firstn" | "table.skip" => {
            if arguments.len() != 2 {
                return Err(format!("{name} requires table and count"));
            }
            let mut table =
                expect_table(eval_expr(&arguments[0], environment, context, diagnostics)?)?;
            let count = expect_usize(eval_expr(&arguments[1], environment, context, diagnostics)?)?;
            let before = table.rows.len();
            if function == "table.firstn" {
                table.rows.truncate(count);
            } else {
                table.rows = table.rows.into_iter().skip(count).collect();
            }
            diagnostic_step(diagnostics, name, before, table.rows.len());
            Ok(RuntimeValue::Table(table))
        }
        "table.group" => {
            if arguments.len() < 3 {
                return Err("Table.Group requires table, keys, and aggregations".to_string());
            }
            let table = expect_table(eval_expr(&arguments[0], environment, context, diagnostics)?)?;
            let keys_value = eval_expr(&arguments[1], environment, context, diagnostics)?;
            let keys = match keys_value {
                RuntimeValue::Json(Value::String(value)) => vec![value],
                value => string_list(value)?,
            };
            for key in &keys {
                if !table.columns.contains(key) {
                    return Err(format!("group key {key} does not exist"));
                }
            }
            let specs = expect_list(eval_expr(&arguments[2], environment, context, diagnostics)?)?;
            let mut aggregates = Vec::new();
            for spec in specs {
                let values = expect_list(spec)?;
                if values.len() < 2 {
                    return Err("group aggregation requires name and lambda".to_string());
                }
                aggregates.push((
                    expect_string(values[0].clone())?,
                    lambda_expr(values[1].clone())?,
                ));
            }
            let mut group_index = HashMap::new();
            let mut groups: Vec<DataTable> = Vec::new();
            for row in &table.rows {
                let key = row_key(row, &keys);
                let index = if let Some(index) = group_index.get(&key) {
                    *index
                } else {
                    let index = groups.len();
                    group_index.insert(key, index);
                    groups.push(DataTable {
                        columns: table.columns.clone(),
                        rows: Vec::new(),
                    });
                    index
                };
                groups[index].rows.push(row.clone());
            }
            let mut rows = Vec::new();
            for group in &groups {
                let first = group
                    .rows
                    .first()
                    .ok_or_else(|| "empty group".to_string())?;
                let mut row = keys
                    .iter()
                    .map(|key| (key.clone(), first.get(key).cloned().unwrap_or(Value::Null)))
                    .collect::<Map<_, _>>();
                for (name, aggregate) in &aggregates {
                    row.insert(
                        name.clone(),
                        runtime_json(eval_expr(
                            aggregate,
                            environment,
                            Some(RowContext::Group(group)),
                            diagnostics,
                        )?)?,
                    );
                }
                rows.push(row);
            }
            let columns = keys
                .iter()
                .cloned()
                .chain(aggregates.iter().map(|(name, _)| name.clone()))
                .collect();
            diagnostic_step(diagnostics, name, table.rows.len(), rows.len());
            Ok(RuntimeValue::Table(DataTable { columns, rows }))
        }
        "table.join" => {
            if arguments.len() < 4 {
                return Err(
                    "Table.Join requires left, left keys, right, and right keys".to_string()
                );
            }
            let left = expect_table(eval_expr(&arguments[0], environment, context, diagnostics)?)?;
            let left_keys_value = eval_expr(&arguments[1], environment, context, diagnostics)?;
            let left_keys = match left_keys_value {
                RuntimeValue::Json(Value::String(value)) => vec![value],
                value => string_list(value)?,
            };
            let right = expect_table(eval_expr(&arguments[2], environment, context, diagnostics)?)?;
            let right_keys_value = eval_expr(&arguments[3], environment, context, diagnostics)?;
            let right_keys = match right_keys_value {
                RuntimeValue::Json(Value::String(value)) => vec![value],
                value => string_list(value)?,
            };
            let kind = arguments
                .get(4)
                .map(|expression| {
                    eval_expr(expression, environment, context, diagnostics).and_then(expect_string)
                })
                .transpose()?
                .unwrap_or_else(|| "JoinKind.Inner".to_string());
            let input_rows = left.rows.len();
            let table = join_tables(left, &left_keys, right, &right_keys, &kind)?;
            diagnostic_step(diagnostics, name, input_rows, table.rows.len());
            Ok(RuntimeValue::Table(table))
        }
        "table.rowcount" => {
            if arguments.len() != 1 {
                return Err("Table.RowCount requires one table".to_string());
            }
            let table = expect_table(eval_expr(&arguments[0], environment, context, diagnostics)?)?;
            Ok(RuntimeValue::Json(Value::Number(Number::from(
                table.rows.len(),
            ))))
        }
        "list.sum" | "list.count" | "list.average" | "list.min" | "list.max" => {
            if arguments.len() != 1 {
                return Err(format!("{name} requires one list"));
            }
            let values = expect_list(eval_expr(&arguments[0], environment, context, diagnostics)?)?;
            list_aggregate(function.as_str(), values)
        }
        "text.lower" | "text.upper" | "text.trim" | "text.from" => {
            if arguments.len() != 1 {
                return Err(format!("{name} requires one value"));
            }
            let value = runtime_json(eval_expr(&arguments[0], environment, context, diagnostics)?)?;
            let text = if let Some(value) = value.as_str() {
                value.to_string()
            } else {
                value.to_string()
            };
            Ok(RuntimeValue::Json(Value::String(match function.as_str() {
                "text.lower" => text.to_lowercase(),
                "text.upper" => text.to_uppercase(),
                "text.trim" => text.trim().to_string(),
                _ => text,
            })))
        }
        "text.contains" | "text.startswith" | "text.endswith" => {
            if arguments.len() < 2 {
                return Err(format!("{name} requires text and substring"));
            }
            let text = expect_string(eval_expr(&arguments[0], environment, context, diagnostics)?)?;
            let needle =
                expect_string(eval_expr(&arguments[1], environment, context, diagnostics)?)?;
            Ok(RuntimeValue::Json(Value::Bool(match function.as_str() {
                "text.contains" => text.contains(&needle),
                "text.startswith" => text.starts_with(&needle),
                _ => text.ends_with(&needle),
            })))
        }
        "number.from" => {
            if arguments.len() != 1 {
                return Err("Number.From requires one value".to_string());
            }
            let value = runtime_json(eval_expr(&arguments[0], environment, context, diagnostics)?)?;
            Ok(RuntimeValue::Json(json_number(
                value_number(&value).ok_or_else(|| format!("cannot convert {value} to number"))?,
            )))
        }
        _ => Err(format!(
            "M function {name} is not available in the safe subset"
        )),
    }
}

fn execute_program(
    program: MProgram,
    inputs: HashMap<String, RuntimeValue>,
    diagnostics: &mut Vec<Value>,
) -> Result<DataTable, String> {
    let mut environment = inputs;
    for (name, expression) in program.bindings {
        let value = eval_expr(&expression, &environment, None, diagnostics)
            .map_err(|error| format!("step {name}: {error}"))?;
        environment.insert(name, value);
    }
    expect_table(eval_expr(&program.output, &environment, None, diagnostics)?)
}

/// Executes a deterministic, side-effect-free M subset over caller-supplied memory/CSV/JSON.
/// Unsupported provider functions return a diagnostic; this function never opens files, sockets,
/// ODBC/OLE DB connections, credential stores, Mashup binaries, or VertiPaq models.
pub(crate) fn execute_m_subset(request: &Value) -> Result<Value, String> {
    let request = request
        .as_object()
        .ok_or_else(|| "M request must be an object".to_string())?;
    let query = request
        .get("m")
        .or_else(|| request.get("query"))
        .and_then(Value::as_str)
        .ok_or_else(|| "M request requires m/query".to_string())?;
    if query.len() > MAX_M_QUERY_BYTES {
        return Err(format!(
            "M query exceeds the safe limit of {MAX_M_QUERY_BYTES} bytes"
        ));
    }
    let mut environment = HashMap::new();
    if let Some(inputs) = request.get("inputs") {
        let inputs = inputs
            .as_object()
            .ok_or_else(|| "inputs must be an object".to_string())?;
        for (name, input) in inputs {
            let table = table_from_input(input)?;
            validate_table_limits(&table, &format!("input {name}"))?;
            environment.insert(name.clone(), RuntimeValue::Table(table));
        }
    }
    if let Some(source) = request.get("source") {
        let table = table_from_input(source)?;
        validate_table_limits(&table, "input Source")?;
        environment.insert("Source".to_string(), RuntimeValue::Table(table));
    }
    let mut diagnostics = vec![
        json!({"severity":"info", "code":"M_SANDBOX", "message":"safe deterministic subset; network, files, credentials, native Mashup and DAX are disabled"}),
    ];
    let result = tokenize_m(query)
        .and_then(|tokens| MParser::new(tokens).parse_program())
        .and_then(|program| execute_program(program, environment, &mut diagnostics));
    match result {
        Ok(table) => Ok(
            json!({"ok":true, "columns":table.columns, "rows":table.json_rows(), "records":table.rows, "diagnostics":diagnostics}),
        ),
        Err(error) => {
            diagnostics
                .push(json!({"severity":"error", "code":"M_EXECUTION_ERROR", "message":error}));
            Ok(
                json!({"ok":false, "columns":[], "rows":[], "records":[], "diagnostics":diagnostics}),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(value: &str) -> Vec<u8> {
        value.as_bytes().to_vec()
    }

    fn fixture() -> BTreeMap<String, Vec<u8>> {
        BTreeMap::from([
            (
                "[Content_Types].xml".to_string(),
                bytes(
                    r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Override PartName="/odd/data/connections.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.connections+xml"/><Override PartName="/odd/query/q.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.queryTable+xml"/><Override PartName="/odd/tables/t.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.table+xml"/><Override PartName="/odd/external/link.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.externalLink+xml"/><Override PartName="/odd/model/item.data" ContentType="application/vnd.ms-excel.model+data"/></Types>"#,
                ),
            ),
            (
                "_rels/.rels".to_string(),
                bytes(
                    r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="book" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="odd/book.xml"/></Relationships>"#,
                ),
            ),
            (
                "odd/book.xml".to_string(),
                bytes(
                    r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"/>"#,
                ),
            ),
            (
                "odd/_rels/book.xml.rels".to_string(),
                bytes(
                    r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="conn-x" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/connections" Target="data/connections.xml"/><Relationship Id="external-x" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/externalLink" Target="external/link.xml"/><Relationship Id="model-x" Type="http://schemas.microsoft.com/office/2007/relationships/model" Target="model/item.data"/></Relationships>"#,
                ),
            ),
            (
                "odd/data/connections.xml".to_string(),
                bytes(
                    r#"<connections xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:v="urn:vendor" v:keep="root"><connection id="7" name="Orders" type="5" refreshOnLoad="0" background="1" v:keep="connection"><dbPr connection="Server=local;User=alice;Password=secret" command="select old" commandType="2" v:keep="db"/><extLst><ext uri="vendor-connection"/></extLst></connection></connections>"#,
                ),
            ),
            (
                "odd/query/q.xml".to_string(),
                bytes(
                    r#"<queryTable xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:v="urn:vendor" name="OrdersQuery" connectionId="7" refreshOnLoad="0" v:keep="query"><queryTableRefresh nextId="3" v:keep="refresh"><queryTableFields count="2"><queryTableField id="1" name="Category" tableColumnId="1"/><queryTableField id="2" name="Amount" tableColumnId="2"/></queryTableFields><extLst><ext uri="vendor-refresh"/></extLst></queryTableRefresh><extLst><ext uri="vendor-query"/></extLst></queryTable>"#,
                ),
            ),
            (
                "odd/tables/t.xml".to_string(),
                bytes(
                    r#"<table xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" id="9" name="Orders" displayName="Orders" ref="A1:B8" v:keep="table" xmlns:v="urn:vendor"><autoFilter ref="A1:B8"/><tableColumns count="2"><tableColumn id="1" name="Category"/><tableColumn id="2" name="Amount"/></tableColumns><extLst><ext uri="vendor-table"/></extLst></table>"#,
                ),
            ),
            (
                "odd/tables/_rels/t.xml.rels".to_string(),
                bytes(
                    r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="query-x" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/queryTable" Target="../query/q.xml"/></Relationships>"#,
                ),
            ),
            (
                "odd/external/link.xml".to_string(),
                bytes(
                    r#"<externalLink xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><externalBook r:id="external-book"><sheetNames><sheetName val="Remote"/></sheetNames><definedNames><definedName name="RemoteName" refersTo="Remote!$A$1"/></definedNames><sheetDataSet><sheetData sheetId="0"><row r="1"><cell r="A1" t="str"><v>cached</v></cell></row></sheetData></sheetDataSet></externalBook></externalLink>"#,
                ),
            ),
            (
                "odd/external/_rels/link.xml.rels".to_string(),
                bytes(
                    r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="external-book" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/externalLinkPath" Target="https://example.invalid/book.xlsx" TargetMode="External"/></Relationships>"#,
                ),
            ),
            ("odd/model/item.data".to_string(), vec![0, 1, 2, 0xff, 0x42]),
            ("odd/vendor/untouched.bin".to_string(), vec![9, 8, 7, 6]),
        ])
    }

    #[test]
    fn resolves_external_data_graph_and_redacts_credentials() {
        let model = inspect_native_data(&fixture()).unwrap();
        assert_eq!(model["connections"][0]["id"], "7");
        assert_eq!(model["connections"][0]["sourceRedacted"], true);
        assert_eq!(
            model["connections"][0]["source"]["connection"],
            "Server=local;User=alice;Password=***"
        );
        assert_eq!(
            model["queryTables"][0]["linkedTables"][0]["loadRange"],
            "A1:B8"
        );
        assert_eq!(model["externalLinks"][0]["sheetNames"][0], "Remote");
        assert_eq!(model["externalLinks"][0]["cachedCellCount"], 1);
        assert_eq!(model["opaqueDataParts"][0]["part"], "odd/model/item.data");
        assert_eq!(model["opaqueDataParts"][0]["opaque"], true);
        assert_eq!(model["dataModel"]["executable"], false);
        assert_eq!(model["dataModel"]["daxRuntime"], false);
        assert_eq!(
            model["dataModel"]["parts"][0]["part"],
            "odd/model/item.data"
        );
        assert!(
            model["dependencies"]
                .as_array()
                .unwrap()
                .iter()
                .any(|relation| relation["targetMode"] == "External")
        );
    }

    #[test]
    fn native_data_edit_is_atomic_lossless_and_updates_load_range() {
        let mut parts = fixture();
        let opaque_before = parts.get("odd/model/item.data").unwrap().clone();
        let vendor_before = parts.get("odd/vendor/untouched.bin").unwrap().clone();
        let external_before = parts.get("odd/external/link.xml").unwrap().clone();
        apply_native_data_edit(&mut parts, &json!({
            "connectionEdits":[{"part":"odd/data/connections.xml", "id":7, "attributes":{"refreshOnLoad":true,"background":false}, "commandText":"select new"}],
            "queryTableEdits":[{"part":"odd/query/q.xml", "attributes":{"refreshOnLoad":true,"preserveFormatting":true}, "refresh":{"minimumVersion":5}, "loadRange":"C2:D20"}]
        })).unwrap();
        let connection =
            std::str::from_utf8(parts.get("odd/data/connections.xml").unwrap()).unwrap();
        assert!(connection.contains("refreshOnLoad=\"1\""));
        assert!(connection.contains("background=\"0\""));
        assert!(connection.contains("command=\"select new\""));
        assert!(connection.contains("Password=secret"));
        assert!(connection.contains("v:keep=\"db\""));
        let query = std::str::from_utf8(parts.get("odd/query/q.xml").unwrap()).unwrap();
        assert!(query.contains("preserveFormatting=\"1\""));
        assert!(query.contains("minimumVersion=\"5\""));
        assert!(query.contains("vendor-query"));
        let table = std::str::from_utf8(parts.get("odd/tables/t.xml").unwrap()).unwrap();
        assert!(table.contains("ref=\"C2:D20\""));
        assert!(table.contains("vendor-table"));
        assert_eq!(parts.get("odd/model/item.data").unwrap(), &opaque_before);
        assert_eq!(
            parts.get("odd/vendor/untouched.bin").unwrap(),
            &vendor_before
        );
        assert_eq!(
            parts.get("odd/external/link.xml").unwrap(),
            &external_before
        );

        let snapshot = parts.clone();
        let error = apply_native_data_edit(
            &mut parts,
            &json!({"queryTableEdits":[{"part":"odd/query/q.xml", "loadRange":"bad range"}]}),
        )
        .unwrap_err();
        assert!(error.contains("invalid queryTable load range"));
        assert_eq!(
            parts, snapshot,
            "failed package edits must not leak partial mutations"
        );

        let stale_error = apply_native_data_edit(
            &mut parts,
            &json!({"connectionEdits":[{"part":"odd/data/connections.xml", "id":7, "expectedSha256":"00", "attributes":{"refreshOnLoad":false}}]}),
        )
        .unwrap_err();
        assert!(stale_error.contains("stale native-data edit"));
        assert_eq!(parts, snapshot);
    }

    #[test]
    fn empty_native_data_patch_is_byte_exact() {
        let mut parts = fixture();
        let before = parts.clone();
        let model = apply_native_data_edit(&mut parts, &json!({})).unwrap();
        assert_eq!(parts, before);
        assert_eq!(model["connections"][0]["name"], "Orders");
    }

    #[test]
    fn safe_m_pipeline_filters_types_adds_groups_and_sorts() {
        let result = execute_m_subset(&json!({
            "m": r#"let
                Source = Input,
                Typed = Table.TransformColumnTypes(Source, {{"Amount", type number}}),
                Filtered = Table.SelectRows(Typed, each [Amount] >= 3),
                Added = Table.AddColumn(Filtered, "Gross", each [Amount] * 2, type number),
                Grouped = Table.Group(Added, {"Category"}, {{"Total", each List.Sum([Gross]), type number}, {"Count", each Table.RowCount(_), Int64.Type}}),
                Sorted = Table.Sort(Grouped, {{"Total", Order.Descending}})
            in Sorted"#,
            "inputs":{"Input":{"columns":["Category","Amount"],"rows":[["A","3"],["B","10"],["A","5"],["C","1"]]}}
        })).unwrap();
        assert_eq!(result["ok"], true, "{}", result);
        assert_eq!(result["columns"], json!(["Category", "Total", "Count"]));
        assert_eq!(result["rows"], json!([["B", 20.0, 1], ["A", 16.0, 2]]));
        assert!(
            result["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["function"] == "Table.Group")
        );
    }

    #[test]
    fn safe_m_join_and_csv_json_inputs_are_deterministic() {
        let result = execute_m_subset(&json!({
            "m":"Table.Join(Left, {\"Id\"}, Right, {\"Id\"}, JoinKind.LeftOuter)",
            "inputs":{
                "Left":{"csv":"Id,Name\r\n1,Alice\r\n2,Bob"},
                "Right":{"json":[{"Id":"1","Score":9}]}
            }
        }))
        .unwrap();
        assert_eq!(result["ok"], true, "{}", result);
        assert_eq!(result["columns"], json!(["Id", "Name", "Id.1", "Score"]));
        assert_eq!(
            result["rows"],
            json!([["1", "Alice", "1", 9], ["2", "Bob", null, null]])
        );

        let blocked =
            execute_m_subset(&json!({"m":"Web.Contents(\"https://example.invalid\")"})).unwrap();
        assert_eq!(blocked["ok"], false);
        assert!(
            blocked["diagnostics"].as_array().unwrap().last().unwrap()["message"]
                .as_str()
                .unwrap()
                .contains("not available in the safe subset")
        );
    }
}
