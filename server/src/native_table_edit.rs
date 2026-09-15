//! Lossless native OOXML table, AutoFilter, and sort-state reader/editor.
//!
//! Excel tables are OPC parts referenced by worksheet relationships.  Their filters and sort
//! state are native SpreadsheetML, while ordinary range filters/sorts live directly in the
//! worksheet.  This module resolves that graph instead of assuming `table1.xml`/`rId1` names and
//! edits only selected byte ranges.  Unknown attributes, extension payloads, namespace prefixes,
//! whitespace, and unrelated relationship records therefore survive byte-for-byte.

use roxmltree::{Document, Node};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ops::Range;

const REL_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const PACKAGE_REL_NS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
const SPREADSHEET_NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const TABLE_CONTENT_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.spreadsheetml.table+xml";
const TABLE_RELATIONSHIP_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/table";

const TABLE_ROOT_CHILD_ORDER: &[&str] = &[
    "autoFilter",
    "sortState",
    "tableColumns",
    "tableStyleInfo",
    "extLst",
];
const WORKSHEET_CHILD_ORDER: &[&str] = &[
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
const TABLE_COLUMN_CHILD_ORDER: &[&str] = &[
    "calculatedColumnFormula",
    "totalsRowFormula",
    "xmlColumnPr",
    "extLst",
];
const AUTO_FILTER_CHILD_ORDER: &[&str] = &["filterColumn", "sortState", "extLst"];
const SORT_STATE_CHILD_ORDER: &[&str] = &["sortCondition", "extLst"];
const FILTER_VARIANTS: &[&str] = &[
    "filters",
    "top10",
    "customFilters",
    "dynamicFilter",
    "colorFilter",
    "iconFilter",
];

#[derive(Clone, Debug)]
struct Relationship {
    id: String,
    kind: String,
    resolved_part: Option<String>,
    external: bool,
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
pub(crate) struct NativeTableWorkbookModel {
    pub workbook_part: String,
    pub tables: Vec<NativeTableModel>,
    pub worksheets: Vec<NativeWorksheetFilterSortModel>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeTableModel {
    pub part: String,
    pub relationship_id: Option<String>,
    pub content_type: Option<String>,
    pub sheet: String,
    pub sheet_id: u64,
    pub sheet_part: String,
    pub id: u64,
    pub name: String,
    pub display_name: String,
    pub reference: String,
    pub header_row_count: u64,
    pub totals_row_count: u64,
    pub totals_row_shown: bool,
    pub insert_row: bool,
    pub insert_row_shift: bool,
    pub published: bool,
    pub attributes: BTreeMap<String, Value>,
    pub columns: Vec<NativeTableColumnModel>,
    pub auto_filter: Option<NativeAutoFilterModel>,
    pub sort_state: Option<NativeSortStateModel>,
    pub style_info: Option<NativeTableStyleModel>,
    pub has_extensions: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeTableColumnModel {
    pub source_index: usize,
    pub id: u64,
    pub name: String,
    pub unique_name: Option<String>,
    pub totals_row_label: Option<String>,
    pub totals_row_function: Option<String>,
    pub query_table_field_id: Option<u64>,
    pub header_row_dxf_id: Option<u64>,
    pub data_dxf_id: Option<u64>,
    pub totals_row_dxf_id: Option<u64>,
    pub calculated_column_formula: Option<String>,
    pub totals_row_formula: Option<String>,
    pub attributes: BTreeMap<String, Value>,
    pub has_xml_column_properties: bool,
    pub has_extensions: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeTableStyleModel {
    pub name: Option<String>,
    pub show_first_column: bool,
    pub show_last_column: bool,
    pub show_row_stripes: bool,
    pub show_column_stripes: bool,
    pub attributes: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeWorksheetFilterSortModel {
    pub sheet: String,
    pub sheet_id: u64,
    pub sheet_part: String,
    pub auto_filter: Option<NativeAutoFilterModel>,
    pub sort_state: Option<NativeSortStateModel>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeAutoFilterModel {
    pub reference: Option<String>,
    pub filter_columns: Vec<NativeFilterColumnModel>,
    pub sort_state: Option<NativeSortStateModel>,
    pub attributes: BTreeMap<String, Value>,
    pub has_extensions: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeFilterColumnModel {
    pub source_index: usize,
    pub column_id: u64,
    pub hidden_button: bool,
    pub show_button: bool,
    pub attributes: BTreeMap<String, Value>,
    pub definition: Option<NativeFilterDefinitionModel>,
    pub has_extensions: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeFilterDefinitionModel {
    pub kind: String,
    pub attributes: BTreeMap<String, Value>,
    pub criteria: Vec<NativeFilterCriterionModel>,
    pub has_extensions: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeFilterCriterionModel {
    pub kind: String,
    pub attributes: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeSortStateModel {
    pub reference: Option<String>,
    pub case_sensitive: bool,
    pub column_sort: bool,
    pub sort_method: Option<String>,
    pub attributes: BTreeMap<String, Value>,
    pub conditions: Vec<NativeSortConditionModel>,
    pub has_extensions: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeSortConditionModel {
    pub source_index: usize,
    pub reference: String,
    pub descending: bool,
    pub sort_by: Option<String>,
    pub dxf_id: Option<u64>,
    pub icon_set: Option<String>,
    pub icon_id: Option<u64>,
    pub custom_list: Option<String>,
    pub attributes: BTreeMap<String, Value>,
}

#[derive(Clone, Debug)]
struct AttributeSpan {
    name: String,
    value: String,
    value_range: Range<usize>,
    full_range: Range<usize>,
}

#[derive(Clone, Debug)]
struct A1Range {
    start_col: u64,
    start_row: u64,
    end_col: u64,
    end_row: u64,
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

fn relationship_part(owner: &str) -> String {
    match owner.rsplit_once('/') {
        Some((directory, file)) => format!("{directory}/_rels/{file}.rels"),
        None => format!("_rels/{owner}.rels"),
    }
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

fn relative_relationship_target(owner: &str, target: &str) -> String {
    let owner_directory: Vec<&str> = part_directory(owner)
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    let target_parts: Vec<&str> = target.split('/').filter(|part| !part.is_empty()).collect();
    let mut common = 0usize;
    while common < owner_directory.len()
        && common < target_parts.len()
        && owner_directory[common] == target_parts[common]
    {
        common += 1;
    }
    let mut result = Vec::new();
    result.extend(std::iter::repeat_n("..", owner_directory.len() - common));
    result.extend(target_parts[common..].iter().copied());
    if result.is_empty() {
        target.rsplit('/').next().unwrap_or(target).to_string()
    } else {
        result.join("/")
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
                external,
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

fn attribute_value(value: &str) -> Value {
    if matches!(value, "true" | "false") {
        Value::Bool(value == "true")
    } else if let Ok(number) = value.parse::<u64>() {
        Value::from(number)
    } else {
        Value::String(value.to_string())
    }
}

fn attribute_map(node: Node<'_, '_>, excluded: &[&str]) -> BTreeMap<String, Value> {
    node.attributes()
        .filter(|attribute| !excluded.contains(&attribute.name()))
        .map(|attribute| {
            let key = attribute
                .namespace()
                .map(|namespace| format!("{{{namespace}}}{}", attribute.name()))
                .unwrap_or_else(|| attribute.name().to_string());
            (key, attribute_value(attribute.value()))
        })
        .collect()
}

fn child_text(node: Node<'_, '_>, name: &str) -> Option<String> {
    direct_child(node, name)
        .and_then(|child| child.text())
        .map(str::to_string)
}

fn parse_filter_definition(node: Node<'_, '_>) -> NativeFilterDefinitionModel {
    let criteria_names: &[&str] = match local_name(node) {
        "filters" => &["filter", "dateGroupItem"],
        "customFilters" => &["customFilter"],
        _ => &[],
    };
    NativeFilterDefinitionModel {
        kind: local_name(node).to_string(),
        attributes: attribute_map(node, &[]),
        criteria: node
            .children()
            .filter(|child| child.is_element() && criteria_names.contains(&local_name(*child)))
            .map(|child| NativeFilterCriterionModel {
                kind: local_name(child).to_string(),
                attributes: attribute_map(child, &[]),
            })
            .collect(),
        has_extensions: direct_child(node, "extLst").is_some(),
    }
}

fn parse_sort_state(node: Node<'_, '_>) -> NativeSortStateModel {
    NativeSortStateModel {
        reference: node.attribute("ref").map(str::to_string),
        case_sensitive: parse_bool(node.attribute("caseSensitive"), false),
        column_sort: parse_bool(node.attribute("columnSort"), false),
        sort_method: node.attribute("sortMethod").map(str::to_string),
        attributes: attribute_map(node, &["ref", "caseSensitive", "columnSort", "sortMethod"]),
        conditions: direct_children(node, "sortCondition")
            .enumerate()
            .map(|(source_index, condition)| NativeSortConditionModel {
                source_index,
                reference: condition.attribute("ref").unwrap_or("").to_string(),
                descending: parse_bool(condition.attribute("descending"), false),
                sort_by: condition.attribute("sortBy").map(str::to_string),
                dxf_id: parse_u64(condition.attribute("dxfId")),
                icon_set: condition.attribute("iconSet").map(str::to_string),
                icon_id: parse_u64(condition.attribute("iconId")),
                custom_list: condition.attribute("customList").map(str::to_string),
                attributes: attribute_map(
                    condition,
                    &[
                        "ref",
                        "descending",
                        "sortBy",
                        "dxfId",
                        "iconSet",
                        "iconId",
                        "customList",
                    ],
                ),
            })
            .collect(),
        has_extensions: direct_child(node, "extLst").is_some(),
    }
}

fn parse_auto_filter(node: Node<'_, '_>) -> NativeAutoFilterModel {
    NativeAutoFilterModel {
        reference: node.attribute("ref").map(str::to_string),
        filter_columns: direct_children(node, "filterColumn")
            .enumerate()
            .map(|(source_index, column)| {
                let definition = column
                    .children()
                    .find(|child| {
                        child.is_element() && FILTER_VARIANTS.contains(&local_name(*child))
                    })
                    .map(parse_filter_definition);
                NativeFilterColumnModel {
                    source_index,
                    column_id: parse_u64(column.attribute("colId")).unwrap_or(0),
                    hidden_button: parse_bool(column.attribute("hiddenButton"), false),
                    show_button: parse_bool(column.attribute("showButton"), true),
                    attributes: attribute_map(column, &["colId", "hiddenButton", "showButton"]),
                    definition,
                    has_extensions: direct_child(column, "extLst").is_some(),
                }
            })
            .collect(),
        sort_state: direct_child(node, "sortState").map(parse_sort_state),
        attributes: attribute_map(node, &["ref"]),
        has_extensions: direct_child(node, "extLst").is_some(),
    }
}

fn parse_table_style(node: Node<'_, '_>) -> NativeTableStyleModel {
    NativeTableStyleModel {
        name: node.attribute("name").map(str::to_string),
        show_first_column: parse_bool(node.attribute("showFirstColumn"), false),
        show_last_column: parse_bool(node.attribute("showLastColumn"), false),
        show_row_stripes: parse_bool(node.attribute("showRowStripes"), false),
        show_column_stripes: parse_bool(node.attribute("showColumnStripes"), false),
        attributes: attribute_map(
            node,
            &[
                "name",
                "showFirstColumn",
                "showLastColumn",
                "showRowStripes",
                "showColumnStripes",
            ],
        ),
    }
}

fn parse_table_part(
    parts: &BTreeMap<String, Vec<u8>>,
    content_types: &ContentTypes,
    sheet: &SheetInfo,
    relationship_id: Option<String>,
    part: &str,
) -> Result<NativeTableModel, String> {
    let (_, document) = parse_xml_part(parts, part)?;
    let root = document.root_element();
    if local_name(root) != "table" {
        return Err(format!("{part} root is not table"));
    }
    let columns = direct_child(root, "tableColumns")
        .into_iter()
        .flat_map(|container| direct_children(container, "tableColumn"))
        .enumerate()
        .map(|(source_index, column)| NativeTableColumnModel {
            source_index,
            id: parse_u64(column.attribute("id")).unwrap_or(0),
            name: column.attribute("name").unwrap_or("").to_string(),
            unique_name: column.attribute("uniqueName").map(str::to_string),
            totals_row_label: column.attribute("totalsRowLabel").map(str::to_string),
            totals_row_function: column.attribute("totalsRowFunction").map(str::to_string),
            query_table_field_id: parse_u64(column.attribute("queryTableFieldId")),
            header_row_dxf_id: parse_u64(column.attribute("headerRowDxfId")),
            data_dxf_id: parse_u64(column.attribute("dataDxfId")),
            totals_row_dxf_id: parse_u64(column.attribute("totalsRowDxfId")),
            calculated_column_formula: child_text(column, "calculatedColumnFormula"),
            totals_row_formula: child_text(column, "totalsRowFormula"),
            attributes: attribute_map(
                column,
                &[
                    "id",
                    "name",
                    "uniqueName",
                    "totalsRowLabel",
                    "totalsRowFunction",
                    "queryTableFieldId",
                    "headerRowDxfId",
                    "dataDxfId",
                    "totalsRowDxfId",
                ],
            ),
            has_xml_column_properties: direct_child(column, "xmlColumnPr").is_some(),
            has_extensions: direct_child(column, "extLst").is_some(),
        })
        .collect();
    Ok(NativeTableModel {
        part: part.to_string(),
        relationship_id,
        content_type: content_type_for(content_types, part),
        sheet: sheet.name.clone(),
        sheet_id: sheet.sheet_id,
        sheet_part: sheet.part.clone(),
        id: parse_u64(root.attribute("id")).unwrap_or(0),
        name: root.attribute("name").unwrap_or("").to_string(),
        display_name: root
            .attribute("displayName")
            .or_else(|| root.attribute("name"))
            .unwrap_or("")
            .to_string(),
        reference: root.attribute("ref").unwrap_or("").to_string(),
        header_row_count: parse_u64(root.attribute("headerRowCount")).unwrap_or(1),
        totals_row_count: parse_u64(root.attribute("totalsRowCount")).unwrap_or(0),
        totals_row_shown: parse_bool(root.attribute("totalsRowShown"), true),
        insert_row: parse_bool(root.attribute("insertRow"), false),
        insert_row_shift: parse_bool(root.attribute("insertRowShift"), false),
        published: parse_bool(root.attribute("published"), false),
        attributes: attribute_map(
            root,
            &[
                "id",
                "name",
                "displayName",
                "ref",
                "headerRowCount",
                "totalsRowCount",
                "totalsRowShown",
                "insertRow",
                "insertRowShift",
                "published",
            ],
        ),
        columns,
        auto_filter: direct_child(root, "autoFilter").map(parse_auto_filter),
        sort_state: direct_child(root, "sortState").map(parse_sort_state),
        style_info: direct_child(root, "tableStyleInfo").map(parse_table_style),
        has_extensions: direct_child(root, "extLst").is_some(),
    })
}

/// Resolves all worksheet table relationships and ordinary worksheet filter/sort state.
pub(crate) fn inspect_native_tables(
    parts: &BTreeMap<String, Vec<u8>>,
) -> Result<NativeTableWorkbookModel, String> {
    let workbook_part = office_document_part(parts)?;
    let (_, workbook_document) = parse_xml_part(parts, &workbook_part)?;
    let workbook_relationships = parse_relationships(parts, &workbook_part)?;
    let sheets = workbook_sheets(
        parts,
        workbook_document.root_element(),
        &workbook_relationships,
    );
    let content_types = parse_content_types(parts)?;
    let mut tables = Vec::new();
    let mut worksheets = Vec::new();
    let mut warnings = Vec::new();
    let mut referenced_parts = HashSet::new();
    for sheet in &sheets {
        let (_, sheet_document) = parse_xml_part(parts, &sheet.part)?;
        let root = sheet_document.root_element();
        worksheets.push(NativeWorksheetFilterSortModel {
            sheet: sheet.name.clone(),
            sheet_id: sheet.sheet_id,
            sheet_part: sheet.part.clone(),
            auto_filter: direct_child(root, "autoFilter").map(parse_auto_filter),
            sort_state: direct_child(root, "sortState").map(parse_sort_state),
        });
        let relationships = parse_relationships(parts, &sheet.part)?;
        let mut sheet_references = HashSet::new();
        if let Some(table_parts) = direct_child(root, "tableParts") {
            for table_part in direct_children(table_parts, "tablePart") {
                let Some(id) = relationship_id(table_part) else {
                    warnings.push(format!("{} has tablePart without r:id", sheet.part));
                    continue;
                };
                let Some(relationship) = relationships.get(&id) else {
                    warnings.push(format!("{} tablePart {id} has no relationship", sheet.part));
                    continue;
                };
                if !relationship.kind.ends_with("/table") {
                    warnings.push(format!("{} relationship {id} is not a table", sheet.part));
                    continue;
                }
                let Some(part) = relationship.resolved_part.as_ref() else {
                    warnings.push(format!(
                        "{} table relationship {id} is external/invalid",
                        sheet.part
                    ));
                    continue;
                };
                if !sheet_references.insert(part.clone()) {
                    warnings.push(format!(
                        "{} references table part {part} more than once",
                        sheet.part
                    ));
                    continue;
                }
                referenced_parts.insert(part.clone());
                tables.push(parse_table_part(
                    parts,
                    &content_types,
                    sheet,
                    Some(id),
                    part,
                )?);
            }
        }
        for relationship in relationships
            .values()
            .filter(|relationship| relationship.kind.ends_with("/table") && !relationship.external)
        {
            if let Some(part) = relationship.resolved_part.as_ref() {
                if !sheet_references.contains(part) {
                    warnings.push(format!(
                        "{} table relationship {} is not present in tableParts",
                        sheet.part, relationship.id
                    ));
                }
            }
        }
    }
    for (part, content_type) in &content_types.overrides {
        if content_type == TABLE_CONTENT_TYPE && !referenced_parts.contains(part) {
            warnings.push(format!("unreferenced table content-type part {part}"));
        }
    }
    let mut ids = HashSet::new();
    let mut names = HashSet::new();
    for table in &tables {
        if table.content_type.as_deref() != Some(TABLE_CONTENT_TYPE) {
            warnings.push(format!(
                "{} has missing/unexpected table content type",
                table.part
            ));
        }
        if !ids.insert(table.id) {
            warnings.push(format!("duplicate table id {}", table.id));
        }
        if !names.insert(table.display_name.to_ascii_lowercase()) {
            warnings.push(format!("duplicate table name {}", table.display_name));
        }
    }
    tables.sort_by(|left, right| {
        left.sheet_id
            .cmp(&right.sheet_id)
            .then_with(|| left.part.cmp(&right.part))
    });
    worksheets.sort_by_key(|sheet| sheet.sheet_id);
    Ok(NativeTableWorkbookModel {
        workbook_part,
        tables,
        worksheets,
        warnings,
    })
}

/// JSON form used by the HTTP API and udoc manifest.
pub(crate) fn parse_native_table_model(parts: &BTreeMap<String, Vec<u8>>) -> Result<Value, String> {
    serde_json::to_value(inspect_native_tables(parts)?).map_err(|error| error.to_string())
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

fn root_closing_tag_start(xml: &str, root: Node<'_, '_>) -> Result<usize, String> {
    let range = root.range();
    xml[range.clone()]
        .rfind("</")
        .map(|relative| range.start + relative)
        .ok_or_else(|| "element has no closing tag".to_string())
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
    let insert = root_closing_tag_start(xml, parent)?;
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
    let requested_order = order
        .iter()
        .position(|name| *name == child_local)
        .unwrap_or(order.len());
    if let Some(next) = parent.children().find(|node| {
        node.is_element()
            && order
                .iter()
                .position(|name| *name == local_name(*node))
                .is_some_and(|position| position > requested_order)
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

fn replace_node(xml: &str, node: Node<'_, '_>, replacement: &str) -> String {
    let mut output = xml.to_string();
    output.replace_range(node.range(), replacement);
    output
}

fn element_text_range(xml: &str, node: Node<'_, '_>) -> Result<Range<usize>, String> {
    let open_end = scan_open_tag_end(xml, node.range().start)?;
    let close_start = root_closing_tag_start(xml, node)?;
    if node
        .children()
        .any(|child| child.is_element() || child.is_comment() || child.is_pi())
    {
        return Err(format!("{} has non-text children", local_name(node)));
    }
    Ok(open_end..close_start)
}

fn set_child_text(
    xml: &str,
    parent: Node<'_, '_>,
    child_name: &str,
    value: Option<&str>,
    order: &[&str],
) -> Result<String, String> {
    if let Some(child) = direct_child(parent, child_name) {
        if let Some(value) = value {
            let range = element_text_range(xml, child)?;
            let escaped = xml_escape_text(value);
            if xml[range.clone()] == escaped {
                return Ok(xml.to_string());
            }
            let mut output = xml.to_string();
            output.replace_range(range, &escaped);
            Ok(output)
        } else {
            Ok(remove_range(xml, child.range()))
        }
    } else if let Some(value) = value {
        let prefix = qname_prefix(open_tag_qname(xml, parent.range().start)?);
        let qname = qualify(prefix, child_name);
        insert_ordered_child(
            xml,
            parent,
            child_name,
            &format!("<{qname}>{}</{qname}>", xml_escape_text(value)),
            order,
        )
    } else {
        Ok(xml.to_string())
    }
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
    if minimum.is_some_and(|minimum| requested < minimum) {
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

fn value_to_attribute(value: &Value, key: &str) -> Result<Option<String>, String> {
    match value {
        Value::Null => Ok(None),
        Value::Bool(value) => Ok(Some(if *value { "1" } else { "0" }.to_string())),
        Value::Number(value) => Ok(Some(value.to_string())),
        Value::String(value) => Ok(Some(value.clone())),
        _ => Err(format!("{key} must be string, number, boolean, or null")),
    }
}

fn generic_attribute_changes(
    object: &Map<String, Value>,
    node: Node<'_, '_>,
    reserved_keys: &[&str],
) -> Result<Vec<(String, Option<String>)>, String> {
    let Some(attributes) = object.get("attributes") else {
        return Ok(Vec::new());
    };
    let attributes = attributes
        .as_object()
        .ok_or_else(|| "attributes must be an object".to_string())?;
    let mut changes = Vec::new();
    for (name, value) in attributes {
        if name.starts_with('{')
            || name.starts_with("xmlns")
            || reserved_keys.contains(&name.as_str())
        {
            return Err(format!(
                "attribute {name} is reserved or namespace-qualified"
            ));
        }
        let requested = value_to_attribute(value, name)?;
        if requested.as_deref() != node.attribute(name.as_str()) {
            changes.push((name.clone(), requested));
        }
    }
    Ok(changes)
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
        .ok_or_else(|| "operation has no string op".to_string())
}

fn selector_source_index(operation: &Map<String, Value>) -> Option<usize> {
    operation
        .get("sourceIndex")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
}

fn object_operations<'a>(
    object: &'a Map<String, Value>,
    key: &str,
) -> Result<Vec<&'a Map<String, Value>>, String> {
    object
        .get(key)
        .map(|value| {
            value
                .as_array()
                .ok_or_else(|| format!("{key} must be an array"))?
                .iter()
                .map(|value| {
                    value
                        .as_object()
                        .ok_or_else(|| format!("every {key} entry must be an object"))
                })
                .collect()
        })
        .unwrap_or_else(|| Ok(Vec::new()))
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
    let start = nodes
        .first()
        .map(|node| node.range().start)
        .ok_or_else(|| "cannot reorder an empty collection".to_string())?;
    let end = nodes.last().unwrap().range().end;
    // Preserve whitespace and unknown nodes between repeated records by moving only each record's
    // own byte range in place.  Reordering is explicitly requested, so inter-record whitespace is
    // intentionally left at its original position.
    let mut output = xml.to_string();
    let mut replacements: Vec<(Range<usize>, String)> = nodes
        .iter()
        .enumerate()
        .map(|(position, node)| (node.range(), raw[order[position]].clone()))
        .collect();
    replacements.sort_by(|left, right| right.0.start.cmp(&left.0.start));
    for (range, replacement) in replacements {
        output.replace_range(range, &replacement);
    }
    debug_assert_eq!(output[start..end].len(), xml[start..end].len());
    Ok(output)
}

/// Replaces only the selected repeated elements, leaving interspersed whitespace, vendor nodes,
/// comments, and extension payloads at their original byte offsets relative to one another.
fn replace_repeated_nodes(xml: &str, nodes: &[Node<'_, '_>], replacement: &str) -> String {
    let Some(first) = nodes.first() else {
        return xml.to_string();
    };
    let insert = first.range().start;
    let mut output = xml.to_string();
    for range in nodes.iter().rev().map(|node| node.range()) {
        output.replace_range(range, "");
    }
    output.insert_str(insert, replacement);
    output
}

fn parse_column_letters(value: &str) -> Option<u64> {
    if value.is_empty() {
        return None;
    }
    let mut column = 0u64;
    for byte in value.bytes() {
        let upper = byte.to_ascii_uppercase();
        if !upper.is_ascii_uppercase() {
            return None;
        }
        column = column.checked_mul(26)?;
        column = column.checked_add(u64::from(upper - b'A' + 1))?;
    }
    Some(column)
}

fn column_letters(mut column: u64) -> String {
    let mut bytes = Vec::new();
    while column > 0 {
        column -= 1;
        bytes.push(b'A' + (column % 26) as u8);
        column /= 26;
    }
    bytes.reverse();
    String::from_utf8(bytes).unwrap_or_default()
}

fn parse_a1_cell(value: &str) -> Option<(u64, u64)> {
    let value = value.trim().trim_matches('$');
    let split = value.find(|character: char| character.is_ascii_digit())?;
    let column = value[..split].replace('$', "");
    let row = value[split..].replace('$', "");
    let column = parse_column_letters(&column)?;
    let row = row.parse::<u64>().ok()?;
    (row > 0 && row <= 1_048_576 && column <= 16_384).then_some((column, row))
}

fn parse_a1_range(value: &str) -> Option<A1Range> {
    let value = value
        .rsplit_once('!')
        .map(|(_, value)| value)
        .unwrap_or(value);
    let (start, end) = value.split_once(':').unwrap_or((value, value));
    let (start_col, start_row) = parse_a1_cell(start)?;
    let (end_col, end_row) = parse_a1_cell(end)?;
    (start_col <= end_col && start_row <= end_row).then_some(A1Range {
        start_col,
        start_row,
        end_col,
        end_row,
    })
}

fn format_a1_range(range: &A1Range) -> String {
    format!(
        "{}{}:{}{}",
        column_letters(range.start_col),
        range.start_row,
        column_letters(range.end_col),
        range.end_row
    )
}

fn table_filter_reference(reference: &str, totals_row_count: u64) -> Option<String> {
    let mut range = parse_a1_range(reference)?;
    range.end_row = range.end_row.checked_sub(totals_row_count)?;
    (range.end_row >= range.start_row).then(|| format_a1_range(&range))
}

fn is_a1_name(value: &str) -> bool {
    parse_a1_cell(value).is_some()
}

fn is_r1c1_name(value: &str) -> bool {
    let upper = value.to_ascii_uppercase();
    let Some(rest) = upper.strip_prefix('R') else {
        return false;
    };
    let Some((row, column)) = rest.split_once('C') else {
        return false;
    };
    !row.is_empty()
        && !column.is_empty()
        && row.bytes().all(|byte| byte.is_ascii_digit())
        && column.bytes().all(|byte| byte.is_ascii_digit())
}

fn validate_table_name(value: &str) -> Result<(), String> {
    if value.is_empty() || value.chars().count() > 255 {
        return Err("table name must contain 1..255 characters".to_string());
    }
    let mut characters = value.chars();
    let first = characters.next().unwrap();
    if !(first == '_' || first == '\\' || first.is_alphabetic())
        || characters
            .any(|character| !(character == '_' || character == '.' || character.is_alphanumeric()))
    {
        return Err(format!("invalid Excel table name {value}"));
    }
    if is_a1_name(value) || is_r1c1_name(value) {
        return Err(format!(
            "table name {value} conflicts with a cell reference"
        ));
    }
    Ok(())
}

fn validate_reference(reference: &str, context: &str) -> Result<A1Range, String> {
    parse_a1_range(reference).ok_or_else(|| format!("{context} is not a valid A1 range"))
}

fn validate_table_shape(
    reference: &str,
    header_row_count: u64,
    totals_row_count: u64,
    columns: usize,
) -> Result<(), String> {
    if header_row_count > 1 || totals_row_count > 1 {
        return Err("headerRowCount and totalsRowCount must be 0 or 1".to_string());
    }
    let range = validate_reference(reference, "table ref")?;
    let width = range.end_col - range.start_col + 1;
    let height = range.end_row - range.start_row + 1;
    if width != columns as u64 {
        return Err(format!(
            "table range width {width} does not match {columns} table columns"
        ));
    }
    if height < header_row_count + totals_row_count {
        return Err("table range is too short for its header/totals rows".to_string());
    }
    Ok(())
}

fn validate_column_names_and_ids(root: Node<'_, '_>) -> Result<(), String> {
    let columns: Vec<Node<'_, '_>> = direct_child(root, "tableColumns")
        .into_iter()
        .flat_map(|container| direct_children(container, "tableColumn"))
        .collect();
    let mut names = HashSet::new();
    let mut ids = HashSet::new();
    for column in columns {
        let name = column.attribute("name").unwrap_or("");
        if name.is_empty() {
            return Err("table column name cannot be empty".to_string());
        }
        if !names.insert(name.to_ascii_lowercase()) {
            return Err(format!("duplicate table column name {name}"));
        }
        let id = parse_u64(column.attribute("id"))
            .filter(|id| *id > 0)
            .ok_or_else(|| format!("table column {name} has invalid id"))?;
        if !ids.insert(id) {
            return Err(format!("duplicate table column id {id}"));
        }
    }
    Ok(())
}

fn map_attribute_fragment(
    object: &Map<String, Value>,
    direct_names: &[&str],
    reserved: &[&str],
) -> Result<String, String> {
    let mut attributes: BTreeMap<String, String> = BTreeMap::new();
    for name in direct_names {
        if let Some(value) = object.get(*name) {
            if let Some(value) = value_to_attribute(value, name)? {
                attributes.insert((*name).to_string(), value);
            }
        }
    }
    if let Some(extra) = object.get("attributes") {
        let extra = extra
            .as_object()
            .ok_or_else(|| "attributes must be an object".to_string())?;
        for (name, value) in extra {
            if name.starts_with('{')
                || name.starts_with("xmlns")
                || reserved.contains(&name.as_str())
            {
                return Err(format!(
                    "attribute {name} is reserved or namespace-qualified"
                ));
            }
            if let Some(value) = value_to_attribute(value, name)? {
                attributes.insert(name.clone(), value);
            }
        }
    }
    Ok(attributes
        .into_iter()
        .map(|(name, value)| format!(" {name}=\"{}\"", xml_escape_attribute(&value)))
        .collect())
}

fn validate_raw_element(raw: &str, expected: Option<&[&str]>) -> Result<String, String> {
    let document = Document::parse(raw).map_err(|error| format!("rawXml XML: {error}"))?;
    let root = document.root_element();
    if let Some(expected) = expected {
        if !expected.contains(&local_name(root)) {
            return Err(format!(
                "rawXml root {} is not one of {}",
                local_name(root),
                expected.join(", ")
            ));
        }
    }
    if root.range() != (0..raw.len()) {
        let leading = &raw[..root.range().start];
        let trailing = &raw[root.range().end..];
        if !leading.trim().is_empty() || !trailing.trim().is_empty() {
            return Err("rawXml must contain exactly one element".to_string());
        }
    }
    Ok(raw.to_string())
}

fn filter_definition_request<'a>(
    payload: &'a Map<String, Value>,
) -> Result<Option<(&'a str, &'a Value)>, String> {
    let requested: Vec<(&str, &Value)> = FILTER_VARIANTS
        .iter()
        .filter_map(|kind| payload.get(*kind).map(|value| (*kind, value)))
        .collect();
    if requested.len() > 1 {
        return Err("a filterColumn can have only one filter definition".to_string());
    }
    Ok(requested.into_iter().next())
}

fn filter_criterion_fragment(prefix: &str, kind: &str, value: &Value) -> Result<String, String> {
    let qname = qualify(prefix, kind);
    match value {
        Value::String(value) if kind == "filter" => Ok(format!(
            "<{qname} val=\"{}\"/>",
            xml_escape_attribute(value)
        )),
        Value::Object(object) => {
            let names: &[&str] = match kind {
                "filter" => &["val"],
                "dateGroupItem" => &[
                    "year",
                    "month",
                    "day",
                    "hour",
                    "minute",
                    "second",
                    "dateTimeGrouping",
                ],
                "customFilter" => &["operator", "val"],
                _ => &[],
            };
            let attrs = map_attribute_fragment(object, names, &[])?;
            if kind == "filter" && !object.contains_key("val") {
                return Err("filter criterion requires val".to_string());
            }
            if kind == "dateGroupItem"
                && !object.contains_key("dateTimeGrouping")
                && !object
                    .get("attributes")
                    .and_then(Value::as_object)
                    .is_some_and(|attrs| attrs.contains_key("dateTimeGrouping"))
            {
                return Err("dateGroupItem requires dateTimeGrouping".to_string());
            }
            if kind == "customFilter" && !object.contains_key("val") {
                return Err("customFilter requires val".to_string());
            }
            Ok(format!("<{qname}{attrs}/>"))
        }
        _ => Err(format!("invalid {kind} criterion")),
    }
}

fn build_filter_definition(prefix: &str, kind: &str, value: &Value) -> Result<String, String> {
    if let Some(raw) = value
        .as_object()
        .and_then(|object| object.get("rawXml"))
        .and_then(Value::as_str)
    {
        return validate_raw_element(raw, Some(FILTER_VARIANTS));
    }
    let object = value
        .as_object()
        .ok_or_else(|| format!("{kind} must be an object or null"))?;
    let direct_names: &[&str] = match kind {
        "filters" => &["blank", "calendarType"],
        "customFilters" => &["and"],
        "dynamicFilter" => &["type", "val", "maxVal"],
        "top10" => &["top", "percent", "val", "filterVal"],
        "colorFilter" => &["dxfId", "cellColor"],
        "iconFilter" => &["iconSet", "iconId"],
        _ => return Err(format!("unsupported filter definition {kind}")),
    };
    let attrs = map_attribute_fragment(object, direct_names, &[])?;
    let qname = qualify(prefix, kind);
    let mut children = String::new();
    if kind == "filters" {
        if let Some(values) = object.get("values") {
            for value in values
                .as_array()
                .ok_or_else(|| "filters.values must be an array".to_string())?
            {
                children.push_str(&filter_criterion_fragment(prefix, "filter", value)?);
            }
        }
        if let Some(groups) = object.get("dateGroups") {
            for value in groups
                .as_array()
                .ok_or_else(|| "filters.dateGroups must be an array".to_string())?
            {
                children.push_str(&filter_criterion_fragment(prefix, "dateGroupItem", value)?);
            }
        }
    } else if kind == "customFilters" {
        if let Some(conditions) = object.get("conditions") {
            let conditions = conditions
                .as_array()
                .ok_or_else(|| "customFilters.conditions must be an array".to_string())?;
            if conditions.len() > 2 {
                return Err("customFilters supports at most two conditions".to_string());
            }
            for value in conditions {
                children.push_str(&filter_criterion_fragment(prefix, "customFilter", value)?);
            }
        }
    }
    if children.is_empty() {
        Ok(format!("<{qname}{attrs}/>"))
    } else {
        Ok(format!("<{qname}{attrs}>{children}</{qname}>"))
    }
}

fn patch_repeated_criteria(
    xml: &str,
    definition: Node<'_, '_>,
    key: &str,
    child_kind: &str,
    values: &Value,
) -> Result<String, String> {
    let values = values
        .as_array()
        .ok_or_else(|| format!("{key} must be an array"))?;
    if child_kind == "customFilter" && values.len() > 2 {
        return Err("customFilters supports at most two conditions".to_string());
    }
    let prefix = qname_prefix(open_tag_qname(xml, definition.range().start)?);
    let replacement: String = values
        .iter()
        .map(|value| filter_criterion_fragment(prefix, child_kind, value))
        .collect::<Result<Vec<_>, _>>()?
        .concat();
    let nodes: Vec<Node<'_, '_>> = direct_children(definition, child_kind).collect();
    if !nodes.is_empty() {
        Ok(replace_repeated_nodes(xml, &nodes, &replacement))
    } else if replacement.is_empty() {
        Ok(xml.to_string())
    } else if let Some(ext) = direct_child(definition, "extLst") {
        let mut output = xml.to_string();
        output.insert_str(ext.range().start, &replacement);
        Ok(output)
    } else {
        insert_child_before_close(xml, definition, &replacement)
    }
}

fn patch_filter_definition(
    xml: &str,
    definition: Node<'_, '_>,
    payload: &Map<String, Value>,
) -> Result<String, String> {
    let kind = local_name(definition);
    let definition_start = definition.range().start;
    let direct_names: &[&str] = match kind {
        "filters" => &["blank", "calendarType"],
        "customFilters" => &["and"],
        "dynamicFilter" => &["type", "val", "maxVal"],
        "top10" => &["top", "percent", "val", "filterVal"],
        "colorFilter" => &["dxfId", "cellColor"],
        "iconFilter" => &["iconSet", "iconId"],
        _ => return Err(format!("unsupported filter definition {kind}")),
    };
    let mut changes = generic_attribute_changes(payload, definition, direct_names)?;
    for name in direct_names {
        if let Some(value) = payload.get(*name) {
            let requested = value_to_attribute(value, name)?;
            if requested.as_deref() != definition.attribute(*name) {
                changes.push(((*name).to_string(), requested));
            }
        }
    }
    let mut output = patch_open_tag(xml, definition.range().start, &changes)?;
    if kind == "filters" {
        if let Some(values) = payload.get("values") {
            let document =
                Document::parse(&output).map_err(|error| format!("AutoFilter XML: {error}"))?;
            let current = document
                .descendants()
                .find(|node| {
                    node.is_element()
                        && local_name(*node) == kind
                        && node.range().start == definition_start
                })
                .ok_or_else(|| format!("{kind} disappeared while editing"))?;
            output = patch_repeated_criteria(&output, current, "values", "filter", values)?;
        }
        if let Some(groups) = payload.get("dateGroups") {
            let document =
                Document::parse(&output).map_err(|error| format!("AutoFilter XML: {error}"))?;
            let current = document
                .descendants()
                .find(|node| {
                    node.is_element()
                        && local_name(*node) == kind
                        && node.range().start == definition_start
                })
                .ok_or_else(|| format!("{kind} disappeared while editing"))?;
            output =
                patch_repeated_criteria(&output, current, "dateGroups", "dateGroupItem", groups)?;
        }
    } else if kind == "customFilters" {
        if let Some(conditions) = payload.get("conditions") {
            let document =
                Document::parse(&output).map_err(|error| format!("AutoFilter XML: {error}"))?;
            let current = document
                .descendants()
                .find(|node| {
                    node.is_element()
                        && local_name(*node) == kind
                        && node.range().start == definition_start
                })
                .ok_or_else(|| format!("{kind} disappeared while editing"))?;
            output = patch_repeated_criteria(
                &output,
                current,
                "conditions",
                "customFilter",
                conditions,
            )?;
        }
    }
    Ok(output)
}

fn selected_filter_column<'a, 'input>(
    auto_filter: Node<'a, 'input>,
    operation: &Map<String, Value>,
) -> Result<Node<'a, 'input>, String> {
    let columns: Vec<Node<'a, 'input>> = direct_children(auto_filter, "filterColumn").collect();
    if let Some(index) = selector_source_index(operation) {
        return columns
            .get(index)
            .copied()
            .ok_or_else(|| format!("filterColumn sourceIndex {index} is missing"));
    }
    if let Some(column_id) = operation.get("colId").and_then(Value::as_u64) {
        return columns
            .into_iter()
            .find(|column| parse_u64(column.attribute("colId")) == Some(column_id))
            .ok_or_else(|| format!("filterColumn colId {column_id} is missing"));
    }
    Err("filterColumn selector requires sourceIndex or colId".to_string())
}

fn filter_column_fragment(prefix: &str, payload: &Map<String, Value>) -> Result<String, String> {
    if let Some(raw) = payload.get("rawXml").and_then(Value::as_str) {
        return validate_raw_element(raw, Some(&["filterColumn"]));
    }
    let _column_id = payload
        .get("colId")
        .and_then(Value::as_u64)
        .ok_or_else(|| "new filterColumn requires colId".to_string())?;
    let attrs = map_attribute_fragment(
        payload,
        &["colId", "hiddenButton", "showButton"],
        &["colId", "hiddenButton", "showButton"],
    )?;
    let definition = filter_definition_request(payload)?
        .map(|(kind, value)| build_filter_definition(prefix, kind, value))
        .transpose()?
        .unwrap_or_default();
    let qname = qualify(prefix, "filterColumn");
    if definition.is_empty() {
        Ok(format!("<{qname}{attrs}/>"))
    } else {
        Ok(format!("<{qname}{attrs}>{definition}</{qname}>"))
    }
}

fn apply_filter_column_operation(
    xml: &str,
    operation: &Map<String, Value>,
) -> Result<String, String> {
    let document = Document::parse(xml).map_err(|error| format!("AutoFilter XML: {error}"))?;
    let auto_filter = document
        .descendants()
        .find(|node| node.is_element() && local_name(*node) == "autoFilter")
        .ok_or_else(|| "autoFilter is missing".to_string())?;
    match operation_name(operation)? {
        "update" => {
            let column = selected_filter_column(auto_filter, operation)?;
            let stable_index = direct_children(auto_filter, "filterColumn")
                .position(|candidate| candidate.range() == column.range())
                .unwrap_or(0);
            let stable_selector = Map::from_iter([
                ("op".to_string(), Value::String("update".to_string())),
                ("sourceIndex".to_string(), Value::from(stable_index as u64)),
            ]);
            let payload = operation_payload(operation)?;
            let mut changes = generic_attribute_changes(
                payload,
                column,
                &["colId", "hiddenButton", "showButton"],
            )?;
            if let Some(change) =
                u64_change(payload, "colId", column, "colId", None, Some(0), false)?
            {
                changes.push(change);
            }
            for (key, attribute, default) in [
                ("hiddenButton", "hiddenButton", false),
                ("showButton", "showButton", true),
            ] {
                if let Some(change) = bool_change(payload, key, column, attribute, default)? {
                    changes.push(change);
                }
            }
            let mut output = patch_open_tag(xml, column.range().start, &changes)?;
            if payload.get("clearFilter").and_then(Value::as_bool) == Some(true) {
                let document =
                    Document::parse(&output).map_err(|error| format!("AutoFilter XML: {error}"))?;
                let auto_filter = document
                    .descendants()
                    .find(|node| node.is_element() && local_name(*node) == "autoFilter")
                    .ok_or_else(|| "autoFilter disappeared".to_string())?;
                let selected = selected_filter_column(auto_filter, &stable_selector)?;
                let ranges: Vec<Range<usize>> = selected
                    .children()
                    .filter(|child| {
                        child.is_element() && FILTER_VARIANTS.contains(&local_name(*child))
                    })
                    .map(|child| child.range())
                    .collect();
                for range in ranges.into_iter().rev() {
                    output.replace_range(range, "");
                }
            }
            if let Some((kind, value)) = filter_definition_request(payload)? {
                let document =
                    Document::parse(&output).map_err(|error| format!("AutoFilter XML: {error}"))?;
                let auto_filter = document
                    .descendants()
                    .find(|node| node.is_element() && local_name(*node) == "autoFilter")
                    .ok_or_else(|| "autoFilter disappeared".to_string())?;
                let column = selected_filter_column(auto_filter, &stable_selector)?;
                let existing: Vec<Node<'_, '_>> = column
                    .children()
                    .filter(|child| {
                        child.is_element() && FILTER_VARIANTS.contains(&local_name(*child))
                    })
                    .collect();
                if value.is_null() {
                    let ranges: Vec<Range<usize>> =
                        existing.into_iter().map(|node| node.range()).collect();
                    for range in ranges.into_iter().rev() {
                        output.replace_range(range, "");
                    }
                } else if existing.len() == 1 && local_name(existing[0]) == kind {
                    let object = value
                        .as_object()
                        .ok_or_else(|| format!("{kind} must be an object or null"))?;
                    output = patch_filter_definition(&output, existing[0], object)?;
                } else {
                    let prefix = qname_prefix(open_tag_qname(&output, column.range().start)?);
                    let replacement = build_filter_definition(prefix, kind, value)?;
                    if !existing.is_empty() {
                        output = replace_repeated_nodes(&output, &existing, &replacement);
                    } else if let Some(ext) = direct_child(column, "extLst") {
                        output.insert_str(ext.range().start, &replacement);
                    } else {
                        output = insert_child_before_close(&output, column, &replacement)?;
                    }
                }
            }
            Ok(output)
        }
        "add" => {
            let payload = operation_payload(operation)?;
            let prefix = qname_prefix(open_tag_qname(xml, auto_filter.range().start)?);
            let fragment = filter_column_fragment(prefix, payload)?;
            insert_ordered_child(
                xml,
                auto_filter,
                "filterColumn",
                &fragment,
                AUTO_FILTER_CHILD_ORDER,
            )
        }
        "delete" => {
            let column = selected_filter_column(auto_filter, operation)?;
            Ok(remove_range(xml, column.range()))
        }
        "reorder" => {
            let columns: Vec<Node<'_, '_>> = direct_children(auto_filter, "filterColumn").collect();
            reorder_nodes(xml, &columns, &operation_order(operation)?)
        }
        op => Err(format!("unsupported filterColumn operation {op}")),
    }
}

fn selected_sort_condition<'a, 'input>(
    sort_state: Node<'a, 'input>,
    operation: &Map<String, Value>,
) -> Result<Node<'a, 'input>, String> {
    let conditions: Vec<Node<'a, 'input>> = direct_children(sort_state, "sortCondition").collect();
    if let Some(index) = selector_source_index(operation) {
        return conditions
            .get(index)
            .copied()
            .ok_or_else(|| format!("sortCondition sourceIndex {index} is missing"));
    }
    if let Some(reference) = operation.get("ref").and_then(Value::as_str) {
        return conditions
            .into_iter()
            .find(|condition| condition.attribute("ref") == Some(reference))
            .ok_or_else(|| format!("sortCondition ref {reference} is missing"));
    }
    Err("sortCondition selector requires sourceIndex or ref".to_string())
}

fn sort_condition_fragment(prefix: &str, payload: &Map<String, Value>) -> Result<String, String> {
    if let Some(raw) = payload.get("rawXml").and_then(Value::as_str) {
        return validate_raw_element(raw, Some(&["sortCondition"]));
    }
    let reference = payload
        .get("ref")
        .and_then(Value::as_str)
        .ok_or_else(|| "new sortCondition requires ref".to_string())?;
    validate_reference(reference, "sortCondition ref")?;
    let attrs = map_attribute_fragment(
        payload,
        &[
            "ref",
            "descending",
            "sortBy",
            "dxfId",
            "iconSet",
            "iconId",
            "customList",
        ],
        &[],
    )?;
    Ok(format!("<{}{attrs}/>", qualify(prefix, "sortCondition")))
}

fn apply_sort_condition_operation(
    xml: &str,
    sort_selector: usize,
    operation: &Map<String, Value>,
) -> Result<String, String> {
    let document = Document::parse(xml).map_err(|error| format!("SortState XML: {error}"))?;
    let sort_states: Vec<Node<'_, '_>> = document
        .descendants()
        .filter(|node| node.is_element() && local_name(*node) == "sortState")
        .collect();
    let sort_state = sort_states
        .get(sort_selector)
        .copied()
        .ok_or_else(|| "sortState disappeared while editing".to_string())?;
    match operation_name(operation)? {
        "update" => {
            let condition = selected_sort_condition(sort_state, operation)?;
            let payload = operation_payload(operation)?;
            let reserved = [
                "ref",
                "descending",
                "sortBy",
                "dxfId",
                "iconSet",
                "iconId",
                "customList",
            ];
            let mut changes = generic_attribute_changes(payload, condition, &reserved)?;
            if let Some(change) = string_change(payload, "ref", condition, "ref", false, true)? {
                if let Some(reference) = change.1.as_deref() {
                    validate_reference(reference, "sortCondition ref")?;
                }
                changes.push(change);
            }
            if let Some(change) =
                bool_change(payload, "descending", condition, "descending", false)?
            {
                changes.push(change);
            }
            for (key, attribute) in [
                ("sortBy", "sortBy"),
                ("iconSet", "iconSet"),
                ("customList", "customList"),
            ] {
                if let Some(change) =
                    string_change(payload, key, condition, attribute, true, false)?
                {
                    changes.push(change);
                }
            }
            for (key, attribute) in [("dxfId", "dxfId"), ("iconId", "iconId")] {
                if let Some(change) =
                    u64_change(payload, key, condition, attribute, None, Some(0), true)?
                {
                    changes.push(change);
                }
            }
            patch_open_tag(xml, condition.range().start, &changes)
        }
        "add" => {
            let payload = operation_payload(operation)?;
            let prefix = qname_prefix(open_tag_qname(xml, sort_state.range().start)?);
            let fragment = sort_condition_fragment(prefix, payload)?;
            insert_ordered_child(
                xml,
                sort_state,
                "sortCondition",
                &fragment,
                SORT_STATE_CHILD_ORDER,
            )
        }
        "delete" => {
            let condition = selected_sort_condition(sort_state, operation)?;
            Ok(remove_range(xml, condition.range()))
        }
        "reorder" => {
            let conditions: Vec<Node<'_, '_>> =
                direct_children(sort_state, "sortCondition").collect();
            reorder_nodes(xml, &conditions, &operation_order(operation)?)
        }
        op => Err(format!("unsupported sortCondition operation {op}")),
    }
}

fn sort_state_fragment(prefix: &str, payload: &Map<String, Value>) -> Result<String, String> {
    if let Some(raw) = payload.get("rawXml").and_then(Value::as_str) {
        return validate_raw_element(raw, Some(&["sortState"]));
    }
    let attrs = map_attribute_fragment(
        payload,
        &["ref", "caseSensitive", "columnSort", "sortMethod"],
        &[],
    )?;
    let mut children = String::new();
    if let Some(conditions) = payload.get("conditions") {
        for condition in conditions
            .as_array()
            .ok_or_else(|| "sortState.conditions must be an array".to_string())?
        {
            children.push_str(&sort_condition_fragment(
                prefix,
                condition
                    .as_object()
                    .ok_or_else(|| "every sort condition must be an object".to_string())?,
            )?);
        }
    }
    let qname = qualify(prefix, "sortState");
    if children.is_empty() {
        Ok(format!("<{qname}{attrs}/>"))
    } else {
        Ok(format!("<{qname}{attrs}>{children}</{qname}>"))
    }
}

fn apply_sort_state_patch_at(
    xml: &str,
    sort_selector: usize,
    patch: &Map<String, Value>,
) -> Result<String, String> {
    let document = Document::parse(xml).map_err(|error| format!("SortState XML: {error}"))?;
    let sort_states: Vec<Node<'_, '_>> = document
        .descendants()
        .filter(|node| node.is_element() && local_name(*node) == "sortState")
        .collect();
    let sort_state = sort_states
        .get(sort_selector)
        .copied()
        .ok_or_else(|| "sortState is missing".to_string())?;
    let mut changes = generic_attribute_changes(
        patch,
        sort_state,
        &["ref", "caseSensitive", "columnSort", "sortMethod"],
    )?;
    if let Some(change) = string_change(patch, "ref", sort_state, "ref", true, true)? {
        if let Some(reference) = change.1.as_deref() {
            validate_reference(reference, "sortState ref")?;
        }
        changes.push(change);
    }
    for (key, attribute) in [
        ("caseSensitive", "caseSensitive"),
        ("columnSort", "columnSort"),
    ] {
        if let Some(change) = bool_change(patch, key, sort_state, attribute, false)? {
            changes.push(change);
        }
    }
    if let Some(change) = string_change(patch, "sortMethod", sort_state, "sortMethod", true, true)?
    {
        if change
            .1
            .as_deref()
            .is_some_and(|value| !matches!(value, "stroke" | "pinYin"))
        {
            return Err("sortMethod must be stroke or pinYin".to_string());
        }
        changes.push(change);
    }
    let mut output = patch_open_tag(xml, sort_state.range().start, &changes)?;
    if let Some(conditions) = patch.get("conditions") {
        let document =
            Document::parse(&output).map_err(|error| format!("SortState XML: {error}"))?;
        let sort_states: Vec<Node<'_, '_>> = document
            .descendants()
            .filter(|node| node.is_element() && local_name(*node) == "sortState")
            .collect();
        let sort_state = sort_states
            .get(sort_selector)
            .copied()
            .ok_or_else(|| "sortState disappeared".to_string())?;
        let existing: Vec<Node<'_, '_>> = direct_children(sort_state, "sortCondition").collect();
        let prefix = qname_prefix(open_tag_qname(&output, sort_state.range().start)?);
        let replacement: String = conditions
            .as_array()
            .ok_or_else(|| "sortState.conditions must be an array".to_string())?
            .iter()
            .map(|condition| {
                sort_condition_fragment(
                    prefix,
                    condition
                        .as_object()
                        .ok_or_else(|| "every sort condition must be an object".to_string())?,
                )
            })
            .collect::<Result<Vec<_>, _>>()?
            .concat();
        if !existing.is_empty() {
            output = replace_repeated_nodes(&output, &existing, &replacement);
        } else if !replacement.is_empty() {
            output = insert_ordered_child(
                &output,
                sort_state,
                "sortCondition",
                &replacement,
                SORT_STATE_CHILD_ORDER,
            )?;
        }
    }
    for operation in object_operations(patch, "conditionOperations")? {
        output = apply_sort_condition_operation(&output, sort_selector, operation)?;
    }
    Ok(output)
}

fn auto_filter_fragment(prefix: &str, payload: &Map<String, Value>) -> Result<String, String> {
    if let Some(raw) = payload.get("rawXml").and_then(Value::as_str) {
        return validate_raw_element(raw, Some(&["autoFilter"]));
    }
    if let Some(reference) = payload.get("ref").and_then(Value::as_str) {
        validate_reference(reference, "autoFilter ref")?;
    }
    let attrs = map_attribute_fragment(payload, &["ref"], &[])?;
    let qname = qualify(prefix, "autoFilter");
    let mut children = String::new();
    if let Some(columns) = payload.get("filterColumns") {
        for column in columns
            .as_array()
            .ok_or_else(|| "autoFilter.filterColumns must be an array".to_string())?
        {
            children.push_str(&filter_column_fragment(
                prefix,
                column
                    .as_object()
                    .ok_or_else(|| "every filter column must be an object".to_string())?,
            )?);
        }
    }
    if let Some(sort) = payload.get("sortState") {
        if !sort.is_null() {
            children.push_str(&sort_state_fragment(
                prefix,
                sort.as_object()
                    .ok_or_else(|| "sortState must be an object or null".to_string())?,
            )?);
        }
    }
    if children.is_empty() {
        Ok(format!("<{qname}{attrs}/>"))
    } else {
        Ok(format!("<{qname}{attrs}>{children}</{qname}>"))
    }
}

fn apply_auto_filter_patch_at(
    xml: &str,
    auto_selector: usize,
    patch: &Map<String, Value>,
) -> Result<String, String> {
    let document = Document::parse(xml).map_err(|error| format!("AutoFilter XML: {error}"))?;
    let filters: Vec<Node<'_, '_>> = document
        .descendants()
        .filter(|node| node.is_element() && local_name(*node) == "autoFilter")
        .collect();
    let auto_filter = filters
        .get(auto_selector)
        .copied()
        .ok_or_else(|| "autoFilter is missing".to_string())?;
    let mut changes = generic_attribute_changes(patch, auto_filter, &["ref"])?;
    if let Some(change) = string_change(patch, "ref", auto_filter, "ref", true, true)? {
        if let Some(reference) = change.1.as_deref() {
            validate_reference(reference, "autoFilter ref")?;
        }
        changes.push(change);
    }
    let mut output = patch_open_tag(xml, auto_filter.range().start, &changes)?;
    if let Some(columns) = patch.get("filterColumns") {
        let document =
            Document::parse(&output).map_err(|error| format!("AutoFilter XML: {error}"))?;
        let filters: Vec<Node<'_, '_>> = document
            .descendants()
            .filter(|node| node.is_element() && local_name(*node) == "autoFilter")
            .collect();
        let auto_filter = filters
            .get(auto_selector)
            .copied()
            .ok_or_else(|| "autoFilter disappeared".to_string())?;
        let existing: Vec<Node<'_, '_>> = direct_children(auto_filter, "filterColumn").collect();
        let prefix = qname_prefix(open_tag_qname(&output, auto_filter.range().start)?);
        let replacement: String = columns
            .as_array()
            .ok_or_else(|| "autoFilter.filterColumns must be an array".to_string())?
            .iter()
            .map(|column| {
                filter_column_fragment(
                    prefix,
                    column
                        .as_object()
                        .ok_or_else(|| "every filter column must be an object".to_string())?,
                )
            })
            .collect::<Result<Vec<_>, _>>()?
            .concat();
        if !existing.is_empty() {
            output = replace_repeated_nodes(&output, &existing, &replacement);
        } else if !replacement.is_empty() {
            output = insert_ordered_child(
                &output,
                auto_filter,
                "filterColumn",
                &replacement,
                AUTO_FILTER_CHILD_ORDER,
            )?;
        }
    }
    for operation in object_operations(patch, "filterColumnOperations")? {
        // The operation helper addresses the first autoFilter in its fragment.  For table and
        // worksheet XML there is at most one top-level autoFilter; nested extension markup is not
        // selected because the first native element is the one being patched.
        output = apply_filter_column_operation(&output, operation)?;
    }
    if let Some(sort_patch) = patch.get("sortState") {
        let document =
            Document::parse(&output).map_err(|error| format!("AutoFilter XML: {error}"))?;
        let filters: Vec<Node<'_, '_>> = document
            .descendants()
            .filter(|node| node.is_element() && local_name(*node) == "autoFilter")
            .collect();
        let auto_filter = filters
            .get(auto_selector)
            .copied()
            .ok_or_else(|| "autoFilter disappeared".to_string())?;
        if let Some(sort_state) = direct_child(auto_filter, "sortState") {
            if sort_patch.is_null() {
                output = remove_range(&output, sort_state.range());
            } else {
                let selector = document
                    .descendants()
                    .filter(|node| node.is_element() && local_name(*node) == "sortState")
                    .position(|node| node.range() == sort_state.range())
                    .unwrap_or(0);
                output = apply_sort_state_patch_at(
                    &output,
                    selector,
                    sort_patch
                        .as_object()
                        .ok_or_else(|| "sortState must be an object or null".to_string())?,
                )?;
            }
        } else if !sort_patch.is_null() {
            let prefix = qname_prefix(open_tag_qname(&output, auto_filter.range().start)?);
            let fragment = sort_state_fragment(
                prefix,
                sort_patch
                    .as_object()
                    .ok_or_else(|| "sortState must be an object or null".to_string())?,
            )?;
            output = insert_ordered_child(
                &output,
                auto_filter,
                "sortState",
                &fragment,
                AUTO_FILTER_CHILD_ORDER,
            )?;
        }
    }
    Ok(output)
}

const TABLE_COLUMN_STRING_ATTRS: &[(&str, &str)] = &[
    ("name", "name"),
    ("uniqueName", "uniqueName"),
    ("totalsRowLabel", "totalsRowLabel"),
    ("totalsRowFunction", "totalsRowFunction"),
    ("headerRowCellStyle", "headerRowCellStyle"),
    ("dataCellStyle", "dataCellStyle"),
    ("totalsRowCellStyle", "totalsRowCellStyle"),
];
const TABLE_COLUMN_U64_ATTRS: &[(&str, &str)] = &[
    ("id", "id"),
    ("queryTableFieldId", "queryTableFieldId"),
    ("headerRowDxfId", "headerRowDxfId"),
    ("dataDxfId", "dataDxfId"),
    ("totalsRowDxfId", "totalsRowDxfId"),
];

fn selected_table_column<'a, 'input>(
    root: Node<'a, 'input>,
    operation: &Map<String, Value>,
) -> Result<Node<'a, 'input>, String> {
    let columns_container =
        direct_child(root, "tableColumns").ok_or_else(|| "tableColumns is missing".to_string())?;
    let columns: Vec<Node<'a, 'input>> =
        direct_children(columns_container, "tableColumn").collect();
    if let Some(index) = selector_source_index(operation) {
        return columns
            .get(index)
            .copied()
            .ok_or_else(|| format!("tableColumn sourceIndex {index} is missing"));
    }
    if let Some(id) = operation.get("id").and_then(Value::as_u64) {
        return columns
            .into_iter()
            .find(|column| parse_u64(column.attribute("id")) == Some(id))
            .ok_or_else(|| format!("tableColumn id {id} is missing"));
    }
    if let Some(name) = operation.get("name").and_then(Value::as_str) {
        return columns
            .into_iter()
            .find(|column| {
                column
                    .attribute("name")
                    .is_some_and(|candidate| candidate.eq_ignore_ascii_case(name))
            })
            .ok_or_else(|| format!("tableColumn name {name} is missing"));
    }
    Err("tableColumn selector requires sourceIndex, id, or name".to_string())
}

fn table_column_fragment(prefix: &str, payload: &Map<String, Value>) -> Result<String, String> {
    if let Some(raw) = payload.get("rawXml").and_then(Value::as_str) {
        return validate_raw_element(raw, Some(&["tableColumn"]));
    }
    let id = payload
        .get("id")
        .and_then(Value::as_u64)
        .filter(|id| *id > 0)
        .ok_or_else(|| "new tableColumn requires a positive id".to_string())?;
    let name = payload
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "new tableColumn requires name".to_string())?;
    let mut direct_names: Vec<&str> = TABLE_COLUMN_STRING_ATTRS
        .iter()
        .chain(TABLE_COLUMN_U64_ATTRS.iter())
        .map(|(key, _)| *key)
        .collect();
    direct_names.push("id");
    direct_names.push("name");
    let attrs = map_attribute_fragment(payload, &direct_names, &[])?;
    let qname = qualify(prefix, "tableColumn");
    let mut children = String::new();
    for (key, local) in [
        ("calculatedColumnFormula", "calculatedColumnFormula"),
        ("totalsRowFormula", "totalsRowFormula"),
    ] {
        if let Some(value) = payload.get(key) {
            if let Some(value) = value.as_str() {
                let child_qname = qualify(prefix, local);
                children.push_str(&format!(
                    "<{child_qname}>{}</{child_qname}>",
                    xml_escape_text(value)
                ));
            } else if !value.is_null() {
                return Err(format!("{key} must be a string or null"));
            }
        }
    }
    let _ = (id, name);
    if children.is_empty() {
        Ok(format!("<{qname}{attrs}/>"))
    } else {
        Ok(format!("<{qname}{attrs}>{children}</{qname}>"))
    }
}

fn sync_table_column_count(xml: &str, force: bool) -> Result<String, String> {
    let document = Document::parse(xml).map_err(|error| format!("Table XML: {error}"))?;
    let root = document.root_element();
    let container =
        direct_child(root, "tableColumns").ok_or_else(|| "tableColumns is missing".to_string())?;
    if !force && container.attribute("count").is_none() {
        return Ok(xml.to_string());
    }
    let count = direct_children(container, "tableColumn").count();
    patch_open_tag(
        xml,
        container.range().start,
        &[("count".to_string(), Some(count.to_string()))],
    )
}

fn apply_table_column_operation(
    xml: &str,
    operation: &Map<String, Value>,
) -> Result<(String, bool), String> {
    let document = Document::parse(xml).map_err(|error| format!("Table XML: {error}"))?;
    let root = document.root_element();
    match operation_name(operation)? {
        "update" => {
            let column = selected_table_column(root, operation)?;
            let stable_index = direct_child(root, "tableColumns")
                .into_iter()
                .flat_map(|container| direct_children(container, "tableColumn"))
                .position(|candidate| candidate.range() == column.range())
                .unwrap_or(0);
            let payload = operation_payload(operation)?;
            if let Some(raw) = payload.get("rawXml").and_then(Value::as_str) {
                return Ok((
                    replace_node(
                        xml,
                        column,
                        &validate_raw_element(raw, Some(&["tableColumn"]))?,
                    ),
                    false,
                ));
            }
            let mut reserved: Vec<&str> = TABLE_COLUMN_STRING_ATTRS
                .iter()
                .chain(TABLE_COLUMN_U64_ATTRS.iter())
                .map(|(key, _)| *key)
                .collect();
            reserved.extend(["calculatedColumnFormula", "totalsRowFormula"]);
            let mut changes = generic_attribute_changes(payload, column, &reserved)?;
            for (key, attribute) in TABLE_COLUMN_STRING_ATTRS {
                let required = *key == "name";
                if let Some(change) =
                    string_change(payload, key, column, attribute, !required, required)?
                {
                    changes.push(change);
                }
            }
            for (key, attribute) in TABLE_COLUMN_U64_ATTRS {
                let required = *key == "id";
                if let Some(change) = u64_change(
                    payload,
                    key,
                    column,
                    attribute,
                    None,
                    Some(if required { 1 } else { 0 }),
                    !required,
                )? {
                    changes.push(change);
                }
            }
            let mut output = patch_open_tag(xml, column.range().start, &changes)?;
            for (key, local) in [
                ("calculatedColumnFormula", "calculatedColumnFormula"),
                ("totalsRowFormula", "totalsRowFormula"),
            ] {
                if let Some(value) = payload.get(key) {
                    let requested = if value.is_null() {
                        None
                    } else {
                        Some(
                            value
                                .as_str()
                                .ok_or_else(|| format!("{key} must be a string or null"))?,
                        )
                    };
                    let document =
                        Document::parse(&output).map_err(|error| format!("Table XML: {error}"))?;
                    let root = document.root_element();
                    let current = direct_child(root, "tableColumns")
                        .into_iter()
                        .flat_map(|container| direct_children(container, "tableColumn"))
                        .nth(stable_index)
                        .ok_or_else(|| "tableColumn disappeared while editing".to_string())?;
                    output = set_child_text(
                        &output,
                        current,
                        local,
                        requested,
                        TABLE_COLUMN_CHILD_ORDER,
                    )?;
                }
            }
            Ok((output, false))
        }
        "add" => {
            let payload = operation_payload(operation)?;
            let container = direct_child(root, "tableColumns")
                .ok_or_else(|| "tableColumns is missing".to_string())?;
            let prefix = qname_prefix(open_tag_qname(xml, container.range().start)?);
            let fragment = table_column_fragment(prefix, payload)?;
            Ok((insert_child_before_close(xml, container, &fragment)?, true))
        }
        "delete" => {
            let column = selected_table_column(root, operation)?;
            Ok((remove_range(xml, column.range()), true))
        }
        "reorder" => {
            let container = direct_child(root, "tableColumns")
                .ok_or_else(|| "tableColumns is missing".to_string())?;
            let columns: Vec<Node<'_, '_>> = direct_children(container, "tableColumn").collect();
            Ok((
                reorder_nodes(xml, &columns, &operation_order(operation)?)?,
                true,
            ))
        }
        op => Err(format!("unsupported tableColumn operation {op}")),
    }
}

fn table_style_fragment(prefix: &str, payload: &Map<String, Value>) -> Result<String, String> {
    if let Some(raw) = payload.get("rawXml").and_then(Value::as_str) {
        return validate_raw_element(raw, Some(&["tableStyleInfo"]));
    }
    let attrs = map_attribute_fragment(
        payload,
        &[
            "name",
            "showFirstColumn",
            "showLastColumn",
            "showRowStripes",
            "showColumnStripes",
        ],
        &[],
    )?;
    Ok(format!("<{}{attrs}/>", qualify(prefix, "tableStyleInfo")))
}

fn apply_table_style_patch(
    xml: &str,
    style: Node<'_, '_>,
    patch: &Map<String, Value>,
) -> Result<String, String> {
    if let Some(raw) = patch.get("rawXml").and_then(Value::as_str) {
        return Ok(replace_node(
            xml,
            style,
            &validate_raw_element(raw, Some(&["tableStyleInfo"]))?,
        ));
    }
    let reserved = [
        "name",
        "showFirstColumn",
        "showLastColumn",
        "showRowStripes",
        "showColumnStripes",
    ];
    let mut changes = generic_attribute_changes(patch, style, &reserved)?;
    if let Some(change) = string_change(patch, "name", style, "name", true, false)? {
        changes.push(change);
    }
    for (key, attribute) in [
        ("showFirstColumn", "showFirstColumn"),
        ("showLastColumn", "showLastColumn"),
        ("showRowStripes", "showRowStripes"),
        ("showColumnStripes", "showColumnStripes"),
    ] {
        if let Some(change) = bool_change(patch, key, style, attribute, false)? {
            changes.push(change);
        }
    }
    patch_open_tag(xml, style.range().start, &changes)
}

fn global_element_index(root: Node<'_, '_>, target: Node<'_, '_>, name: &str) -> usize {
    root.descendants()
        .filter(|node| node.is_element() && local_name(*node) == name)
        .position(|node| node.range() == target.range())
        .unwrap_or(0)
}

fn apply_optional_auto_filter(
    xml: &str,
    parent_name: &str,
    value: &Value,
    order: &[&str],
) -> Result<String, String> {
    let document = Document::parse(xml).map_err(|error| format!("Spreadsheet XML: {error}"))?;
    let parent = document.root_element();
    if local_name(parent) != parent_name {
        return Err(format!("expected {parent_name} root"));
    }
    if let Some(auto_filter) = direct_child(parent, "autoFilter") {
        if value.is_null() {
            Ok(remove_range(xml, auto_filter.range()))
        } else {
            let selector = global_element_index(parent, auto_filter, "autoFilter");
            apply_auto_filter_patch_at(
                xml,
                selector,
                value
                    .as_object()
                    .ok_or_else(|| "autoFilter must be an object or null".to_string())?,
            )
        }
    } else if value.is_null() {
        Ok(xml.to_string())
    } else {
        let prefix = qname_prefix(open_tag_qname(xml, parent.range().start)?);
        let fragment = auto_filter_fragment(
            prefix,
            value
                .as_object()
                .ok_or_else(|| "autoFilter must be an object or null".to_string())?,
        )?;
        insert_ordered_child(xml, parent, "autoFilter", &fragment, order)
    }
}

fn apply_optional_direct_sort_state(
    xml: &str,
    parent_name: &str,
    value: &Value,
    order: &[&str],
) -> Result<String, String> {
    let document = Document::parse(xml).map_err(|error| format!("Spreadsheet XML: {error}"))?;
    let parent = document.root_element();
    if local_name(parent) != parent_name {
        return Err(format!("expected {parent_name} root"));
    }
    if let Some(sort_state) = direct_child(parent, "sortState") {
        if value.is_null() {
            Ok(remove_range(xml, sort_state.range()))
        } else {
            let selector = global_element_index(parent, sort_state, "sortState");
            apply_sort_state_patch_at(
                xml,
                selector,
                value
                    .as_object()
                    .ok_or_else(|| "sortState must be an object or null".to_string())?,
            )
        }
    } else if value.is_null() {
        Ok(xml.to_string())
    } else {
        let prefix = qname_prefix(open_tag_qname(xml, parent.range().start)?);
        let fragment = sort_state_fragment(
            prefix,
            value
                .as_object()
                .ok_or_else(|| "sortState must be an object or null".to_string())?,
        )?;
        insert_ordered_child(xml, parent, "sortState", &fragment, order)
    }
}

fn validate_auto_filter_columns(
    auto_filter: Node<'_, '_>,
    width: Option<u64>,
) -> Result<(), String> {
    let mut ids = HashSet::new();
    for column in direct_children(auto_filter, "filterColumn") {
        let id = parse_u64(column.attribute("colId"))
            .ok_or_else(|| "filterColumn requires a non-negative colId".to_string())?;
        if width.is_some_and(|width| id >= width) {
            return Err(format!(
                "filterColumn colId {id} is outside the filtered range"
            ));
        }
        if !ids.insert(id) {
            return Err(format!("duplicate filterColumn colId {id}"));
        }
        let variants = column
            .children()
            .filter(|child| child.is_element() && FILTER_VARIANTS.contains(&local_name(*child)))
            .count();
        if variants > 1 {
            return Err(format!(
                "filterColumn colId {id} has multiple filter definitions"
            ));
        }
        if let Some(custom_filters) = direct_child(column, "customFilters") {
            let count = direct_children(custom_filters, "customFilter").count();
            if count > 2 {
                return Err(format!(
                    "filterColumn colId {id} has more than two customFilter conditions"
                ));
            }
        }
    }
    Ok(())
}

fn validate_sort_state(sort_state: Node<'_, '_>) -> Result<(), String> {
    if let Some(reference) = sort_state.attribute("ref") {
        validate_reference(reference, "sortState ref")?;
    }
    for condition in direct_children(sort_state, "sortCondition") {
        validate_reference(
            condition
                .attribute("ref")
                .ok_or_else(|| "sortCondition requires ref".to_string())?,
            "sortCondition ref",
        )?;
    }
    Ok(())
}

fn validate_table_xml(xml: &str, strict_filter_ref: bool) -> Result<(), String> {
    let document = Document::parse(xml).map_err(|error| format!("Table XML: {error}"))?;
    let root = document.root_element();
    if local_name(root) != "table" {
        return Err("table part root must be table".to_string());
    }
    let id = parse_u64(root.attribute("id"))
        .filter(|id| *id > 0)
        .ok_or_else(|| "table id must be positive".to_string())?;
    let _ = id;
    let name = root
        .attribute("name")
        .ok_or_else(|| "table name is required".to_string())?;
    let display_name = root
        .attribute("displayName")
        .ok_or_else(|| "table displayName is required".to_string())?;
    validate_table_name(name)?;
    validate_table_name(display_name)?;
    let reference = root
        .attribute("ref")
        .ok_or_else(|| "table ref is required".to_string())?;
    let header = parse_u64(root.attribute("headerRowCount")).unwrap_or(1);
    let totals = parse_u64(root.attribute("totalsRowCount")).unwrap_or(0);
    let columns =
        direct_child(root, "tableColumns").ok_or_else(|| "tableColumns is required".to_string())?;
    let count = direct_children(columns, "tableColumn").count();
    validate_table_shape(reference, header, totals, count)?;
    validate_column_names_and_ids(root)?;
    if let Some(declared) = parse_u64(columns.attribute("count")) {
        if declared != count as u64 {
            return Err(format!(
                "tableColumns count {declared} does not match {count} children"
            ));
        }
    }
    if let Some(auto_filter) = direct_child(root, "autoFilter") {
        let table_range = parse_a1_range(reference).unwrap();
        validate_auto_filter_columns(
            auto_filter,
            Some(table_range.end_col - table_range.start_col + 1),
        )?;
        if strict_filter_ref {
            let expected = table_filter_reference(reference, totals)
                .ok_or_else(|| "table has no valid filterable rows".to_string())?;
            if auto_filter.attribute("ref") != Some(expected.as_str()) {
                return Err(format!(
                    "table autoFilter ref must be {expected}, got {}",
                    auto_filter.attribute("ref").unwrap_or("<missing>")
                ));
            }
        }
        if let Some(sort) = direct_child(auto_filter, "sortState") {
            validate_sort_state(sort)?;
        }
    }
    if let Some(sort) = direct_child(root, "sortState") {
        validate_sort_state(sort)?;
    }
    Ok(())
}

/// Applies a lossless differential patch to one native table part.
///
/// Supported top-level keys include root metadata (`name`, `displayName`, `ref`, row counts,
/// style/DXF attributes), `columnOperations`, `autoFilter`, `sortState`, and `styleInfo`.
/// Repeated child records accept `add`, `update`, `delete`, and `reorder`; `rawXml` is available
/// for valid future/extension records.  An empty patch returns the original bytes exactly.
pub(crate) fn apply_table_part_edit(table_xml: &str, patch: &Value) -> Result<String, String> {
    let patch = patch
        .as_object()
        .ok_or_else(|| "table patch must be an object".to_string())?;
    if patch.is_empty() {
        return Ok(table_xml.to_string());
    }
    let document = Document::parse(table_xml).map_err(|error| format!("Table XML: {error}"))?;
    let root = document.root_element();
    if local_name(root) != "table" {
        return Err("table XML root must be table".to_string());
    }
    let root_string_attrs = [
        ("name", "name", false, true),
        ("displayName", "displayName", false, true),
        ("comment", "comment", true, false),
        ("tableType", "tableType", true, true),
        ("headerRowCellStyle", "headerRowCellStyle", true, false),
        ("dataCellStyle", "dataCellStyle", true, false),
        ("totalsRowCellStyle", "totalsRowCellStyle", true, false),
    ];
    let root_u64_attrs = [
        ("id", "id", false, 1),
        ("headerRowCount", "headerRowCount", true, 0),
        ("totalsRowCount", "totalsRowCount", true, 0),
        ("headerRowDxfId", "headerRowDxfId", true, 0),
        ("dataDxfId", "dataDxfId", true, 0),
        ("totalsRowDxfId", "totalsRowDxfId", true, 0),
        ("headerRowBorderDxfId", "headerRowBorderDxfId", true, 0),
        ("tableBorderDxfId", "tableBorderDxfId", true, 0),
        ("totalsRowBorderDxfId", "totalsRowBorderDxfId", true, 0),
        ("connectionId", "connectionId", true, 0),
    ];
    let root_bool_attrs = [
        ("totalsRowShown", "totalsRowShown", true),
        ("insertRow", "insertRow", false),
        ("insertRowShift", "insertRowShift", false),
        ("published", "published", false),
    ];
    let mut reserved = vec![
        "ref",
        "autoFilter",
        "sortState",
        "styleInfo",
        "columnOperations",
        "syncAutoFilter",
    ];
    reserved.extend(root_string_attrs.iter().map(|(key, _, _, _)| *key));
    reserved.extend(root_u64_attrs.iter().map(|(key, _, _, _)| *key));
    reserved.extend(root_bool_attrs.iter().map(|(key, _, _)| *key));
    let mut changes = generic_attribute_changes(patch, root, &reserved)?;
    if let Some(change) = string_change(patch, "ref", root, "ref", false, true)? {
        if let Some(reference) = change.1.as_deref() {
            validate_reference(reference, "table ref")?;
        }
        changes.push(change);
    }
    for (key, attribute, removable, nonempty) in root_string_attrs {
        if let Some(change) = string_change(patch, key, root, attribute, removable, nonempty)? {
            if matches!(key, "name" | "displayName") {
                if let Some(name) = change.1.as_deref() {
                    validate_table_name(name)?;
                }
            }
            changes.push(change);
        }
    }
    for (key, attribute, removable, minimum) in root_u64_attrs {
        let default = match key {
            "headerRowCount" => Some(1),
            "totalsRowCount" => Some(0),
            _ => None,
        };
        if let Some(change) = u64_change(
            patch,
            key,
            root,
            attribute,
            default,
            Some(minimum),
            removable,
        )? {
            changes.push(change);
        }
    }
    for (key, attribute, default) in root_bool_attrs {
        if let Some(change) = bool_change(patch, key, root, attribute, default)? {
            changes.push(change);
        }
    }
    let mut output = patch_open_tag(table_xml, root.range().start, &changes)?;
    let mut structural_columns = false;
    for operation in object_operations(patch, "columnOperations")? {
        let (updated, structural) = apply_table_column_operation(&output, operation)?;
        output = updated;
        structural_columns |= structural;
    }
    if structural_columns {
        output = sync_table_column_count(&output, true)?;
    }
    if let Some(value) = patch.get("autoFilter") {
        output = apply_optional_auto_filter(&output, "table", value, TABLE_ROOT_CHILD_ORDER)?;
    }
    if let Some(value) = patch.get("sortState") {
        output = apply_optional_direct_sort_state(&output, "table", value, TABLE_ROOT_CHILD_ORDER)?;
    }
    if let Some(value) = patch.get("styleInfo") {
        let document = Document::parse(&output).map_err(|error| format!("Table XML: {error}"))?;
        let root = document.root_element();
        if let Some(style) = direct_child(root, "tableStyleInfo") {
            if value.is_null() {
                output = remove_range(&output, style.range());
            } else {
                output = apply_table_style_patch(
                    &output,
                    style,
                    value
                        .as_object()
                        .ok_or_else(|| "styleInfo must be an object or null".to_string())?,
                )?;
            }
        } else if !value.is_null() {
            let prefix = qname_prefix(open_tag_qname(&output, root.range().start)?);
            let fragment = table_style_fragment(
                prefix,
                value
                    .as_object()
                    .ok_or_else(|| "styleInfo must be an object or null".to_string())?,
            )?;
            output = insert_ordered_child(
                &output,
                root,
                "tableStyleInfo",
                &fragment,
                TABLE_ROOT_CHILD_ORDER,
            )?;
        }
    }
    let shape_changed = patch.contains_key("ref")
        || patch.contains_key("headerRowCount")
        || patch.contains_key("totalsRowCount")
        || structural_columns;
    if shape_changed && patch.get("syncAutoFilter").and_then(Value::as_bool) != Some(false) {
        let document = Document::parse(&output).map_err(|error| format!("Table XML: {error}"))?;
        let root = document.root_element();
        if let Some(auto_filter) = direct_child(root, "autoFilter") {
            let reference = root.attribute("ref").unwrap_or("");
            let totals = parse_u64(root.attribute("totalsRowCount")).unwrap_or(0);
            let expected = table_filter_reference(reference, totals)
                .ok_or_else(|| "table has no valid autoFilter range".to_string())?;
            output = patch_open_tag(
                &output,
                auto_filter.range().start,
                &[("ref".to_string(), Some(expected))],
            )?;
        }
    }
    validate_table_xml(&output, shape_changed)?;
    Ok(output)
}

/// Applies native ordinary range AutoFilter/sortState edits to a worksheet without touching
/// sheetData, drawings, validations, conditional formatting, or extension payloads.
pub(crate) fn apply_worksheet_filter_sort_edit(
    worksheet_xml: &str,
    patch: &Value,
) -> Result<String, String> {
    let patch = patch
        .as_object()
        .ok_or_else(|| "worksheet filter/sort patch must be an object".to_string())?;
    if patch.is_empty() {
        return Ok(worksheet_xml.to_string());
    }
    let document =
        Document::parse(worksheet_xml).map_err(|error| format!("Worksheet XML: {error}"))?;
    if local_name(document.root_element()) != "worksheet" {
        return Err("worksheet XML root must be worksheet".to_string());
    }
    let mut output = worksheet_xml.to_string();
    if let Some(value) = patch.get("autoFilter") {
        output = apply_optional_auto_filter(&output, "worksheet", value, WORKSHEET_CHILD_ORDER)?;
    }
    if let Some(value) = patch.get("sortState") {
        output =
            apply_optional_direct_sort_state(&output, "worksheet", value, WORKSHEET_CHILD_ORDER)?;
    }
    let document = Document::parse(&output).map_err(|error| format!("Worksheet XML: {error}"))?;
    let root = document.root_element();
    if let Some(auto_filter) = direct_child(root, "autoFilter") {
        let width = auto_filter
            .attribute("ref")
            .and_then(parse_a1_range)
            .map(|range| range.end_col - range.start_col + 1);
        validate_auto_filter_columns(auto_filter, width)?;
        if let Some(sort) = direct_child(auto_filter, "sortState") {
            validate_sort_state(sort)?;
        }
    }
    if let Some(sort) = direct_child(root, "sortState") {
        validate_sort_state(sort)?;
    }
    Ok(output)
}

#[derive(Clone, Debug)]
struct FormulaHit {
    part: String,
    text_range: Range<usize>,
    text: String,
    cell_reference: Option<String>,
}

fn is_formula_element(name: &str) -> bool {
    matches!(
        name,
        "f" | "definedName"
            | "formula"
            | "formula1"
            | "formula2"
            | "calculatedColumnFormula"
            | "totalsRowFormula"
    )
}

fn collect_formula_hits(parts: &BTreeMap<String, Vec<u8>>) -> Vec<FormulaHit> {
    let mut hits = Vec::new();
    for (part, bytes) in parts {
        if !part.ends_with(".xml") {
            continue;
        }
        let Ok(xml) = std::str::from_utf8(bytes) else {
            continue;
        };
        let Ok(document) = Document::parse(xml) else {
            continue;
        };
        for node in document.descendants().filter(|node| {
            node.is_element()
                && is_formula_element(local_name(*node))
                && !node.children().any(|child| child.is_element())
        }) {
            let Some(text) = node.text() else {
                continue;
            };
            let Ok(text_range) = element_text_range(xml, node) else {
                continue;
            };
            let cell_reference = (local_name(node) == "f")
                .then(|| node.parent_element())
                .flatten()
                .filter(|parent| local_name(*parent) == "c")
                .and_then(|cell| cell.attribute("r"))
                .map(str::to_string);
            hits.push(FormulaHit {
                part: part.clone(),
                text_range,
                text: text.to_string(),
                cell_reference,
            });
        }
    }
    hits
}

fn ascii_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'\\')
}

fn table_qualified_reference(value: &str, table_name: &str) -> bool {
    let value_bytes = value.as_bytes();
    let name_bytes = table_name.as_bytes();
    if name_bytes.is_empty() || value_bytes.len() < name_bytes.len() + 1 {
        return false;
    }
    (0..=value_bytes.len() - name_bytes.len()).any(|start| {
        let end = start + name_bytes.len();
        value_bytes[start..end].eq_ignore_ascii_case(name_bytes)
            && (start == 0 || !ascii_word_byte(value_bytes[start - 1]))
            && value_bytes.get(end) == Some(&b'[')
    })
}

fn replace_table_qualified_reference(value: &str, old: &str, new: &str) -> String {
    let bytes = value.as_bytes();
    let old_bytes = old.as_bytes();
    if old_bytes.is_empty() || bytes.len() < old_bytes.len() + 1 {
        return value.to_string();
    }
    let mut output = String::with_capacity(value.len() + new.len().saturating_sub(old.len()));
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        if cursor + old_bytes.len() < bytes.len()
            && bytes[cursor..cursor + old_bytes.len()].eq_ignore_ascii_case(old_bytes)
            && (cursor == 0 || !ascii_word_byte(bytes[cursor - 1]))
            && bytes.get(cursor + old_bytes.len()) == Some(&b'[')
        {
            output.push_str(new);
            cursor += old_bytes.len();
        } else {
            let character = value[cursor..].chars().next().unwrap();
            output.push(character);
            cursor += character.len_utf8();
        }
    }
    output
}

fn bracket_column_reference(value: &str, column_name: &str) -> bool {
    replace_bracket_column_reference(value, column_name, column_name).1
}

fn replace_bracket_column_reference(value: &str, old: &str, new: &str) -> (String, bool) {
    let mut output = String::with_capacity(value.len() + new.len().saturating_sub(old.len()));
    let mut cursor = 0usize;
    let bytes = value.as_bytes();
    let mut changed = false;
    while cursor < bytes.len() {
        if bytes[cursor] != b'[' {
            let character = value[cursor..].chars().next().unwrap();
            output.push(character);
            cursor += character.len_utf8();
            continue;
        }
        let Some(relative_end) = value[cursor + 1..].find(']') else {
            output.push_str(&value[cursor..]);
            break;
        };
        let end = cursor + 1 + relative_end;
        let inner = &value[cursor + 1..end];
        let (prefix, candidate) = if let Some(candidate) = inner.strip_prefix("@'") {
            ("@'", candidate)
        } else if let Some(candidate) = inner.strip_prefix('@') {
            ("@", candidate)
        } else if let Some(candidate) = inner.strip_prefix('\'') {
            ("'", candidate)
        } else {
            ("", inner)
        };
        output.push('[');
        output.push_str(prefix);
        if candidate.eq_ignore_ascii_case(old) {
            output.push_str(new);
            changed = true;
        } else {
            output.push_str(candidate);
        }
        output.push(']');
        cursor = end + 1;
    }
    (output, changed)
}

fn cell_in_table(reference: Option<&str>, table_reference: &str) -> bool {
    let Some((column, row)) = reference.and_then(parse_a1_cell) else {
        return false;
    };
    let Some(range) = parse_a1_range(table_reference) else {
        return false;
    };
    column >= range.start_col
        && column <= range.end_col
        && row >= range.start_row
        && row <= range.end_row
}

fn formula_is_table_context(hit: &FormulaHit, table: &NativeTableModel) -> bool {
    hit.part == table.part
        || table_qualified_reference(&hit.text, &table.name)
        || table_qualified_reference(&hit.text, &table.display_name)
        || (hit.part == table.sheet_part
            && cell_in_table(hit.cell_reference.as_deref(), &table.reference))
}

fn table_reference_hits<'a>(
    hits: &'a [FormulaHit],
    table: &NativeTableModel,
) -> Vec<&'a FormulaHit> {
    hits.iter()
        .filter(|hit| {
            table_qualified_reference(&hit.text, &table.name)
                || table_qualified_reference(&hit.text, &table.display_name)
        })
        .collect()
}

fn column_reference_hits<'a>(
    hits: &'a [FormulaHit],
    table: &NativeTableModel,
    column: &str,
) -> Vec<&'a FormulaHit> {
    hits.iter()
        .filter(|hit| {
            formula_is_table_context(hit, table) && bracket_column_reference(&hit.text, column)
        })
        .collect()
}

fn rewrite_structured_references(
    parts: &mut BTreeMap<String, Vec<u8>>,
    table: &NativeTableModel,
    new_table_name: Option<&str>,
    column_renames: &[(String, String)],
) -> Result<(), String> {
    let hits = collect_formula_hits(parts);
    let mut replacements: HashMap<String, Vec<(Range<usize>, String)>> = HashMap::new();
    for hit in hits {
        let table_context = formula_is_table_context(&hit, table);
        let mut formula = hit.text.clone();
        if let Some(new_name) = new_table_name {
            formula = replace_table_qualified_reference(&formula, &table.name, new_name);
            if !table.display_name.eq_ignore_ascii_case(&table.name) {
                formula =
                    replace_table_qualified_reference(&formula, &table.display_name, new_name);
            }
        }
        if table_context {
            for (old, new) in column_renames {
                formula = replace_bracket_column_reference(&formula, old, new).0;
            }
        }
        if formula != hit.text {
            replacements
                .entry(hit.part)
                .or_default()
                .push((hit.text_range, xml_escape_text(&formula)));
        }
    }
    for (part, mut part_replacements) in replacements {
        let bytes = parts
            .get(&part)
            .ok_or_else(|| format!("formula part {part} disappeared"))?;
        let mut xml = std::str::from_utf8(bytes)
            .map_err(|error| format!("{part} UTF-8: {error}"))?
            .to_string();
        part_replacements.sort_by(|left, right| right.0.start.cmp(&left.0.start));
        for (range, replacement) in part_replacements {
            xml.replace_range(range, &replacement);
        }
        Document::parse(&xml)
            .map_err(|error| format!("{part} XML after formula rewrite: {error}"))?;
        parts.insert(part, xml.into_bytes());
    }
    Ok(())
}

fn table_slicer_references(parts: &BTreeMap<String, Vec<u8>>, table_id: u64) -> Vec<String> {
    let mut references = Vec::new();
    for (part, bytes) in parts {
        let Ok(xml) = std::str::from_utf8(bytes) else {
            continue;
        };
        let Ok(document) = Document::parse(xml) else {
            continue;
        };
        if document.descendants().any(|node| {
            node.is_element()
                && matches!(
                    local_name(node),
                    "tableSlicerCache" | "tableSlicerCacheDefinition"
                )
                && parse_u64(node.attribute("tableId")) == Some(table_id)
        }) {
            references.push(part.clone());
        }
    }
    references
}

fn ensure_content_type_override(
    parts: &mut BTreeMap<String, Vec<u8>>,
    part: &str,
    content_type: &str,
) -> Result<(), String> {
    let path = "[Content_Types].xml";
    let bytes = parts
        .get(path)
        .ok_or_else(|| "OPC package has no [Content_Types].xml".to_string())?;
    let xml = std::str::from_utf8(bytes)
        .map_err(|error| format!("[Content_Types].xml UTF-8: {error}"))?;
    let document =
        Document::parse(xml).map_err(|error| format!("[Content_Types].xml XML: {error}"))?;
    let root = document.root_element();
    let normalized = normalize_part_path(part).ok_or_else(|| "invalid part path".to_string())?;
    if let Some(existing) = root.children().find(|node| {
        node.is_element()
            && local_name(*node) == "Override"
            && node
                .attribute("PartName")
                .and_then(normalize_part_path)
                .as_deref()
                == Some(normalized.as_str())
    }) {
        let updated = patch_open_tag(
            xml,
            existing.range().start,
            &[("ContentType".to_string(), Some(content_type.to_string()))],
        )?;
        parts.insert(path.to_string(), updated.into_bytes());
        return Ok(());
    }
    let prefix = qname_prefix(open_tag_qname(xml, root.range().start)?);
    let qname = qualify(prefix, "Override");
    let fragment = format!(
        "<{qname} PartName=\"/{}\" ContentType=\"{}\"/>",
        xml_escape_attribute(&normalized),
        xml_escape_attribute(content_type)
    );
    let updated = insert_child_before_close(xml, root, &fragment)?;
    parts.insert(path.to_string(), updated.into_bytes());
    Ok(())
}

fn remove_content_type_override(
    parts: &mut BTreeMap<String, Vec<u8>>,
    part: &str,
) -> Result<(), String> {
    let path = "[Content_Types].xml";
    let Some(bytes) = parts.get(path) else {
        return Ok(());
    };
    let xml = std::str::from_utf8(bytes)
        .map_err(|error| format!("[Content_Types].xml UTF-8: {error}"))?;
    let document =
        Document::parse(xml).map_err(|error| format!("[Content_Types].xml XML: {error}"))?;
    let normalized = normalize_part_path(part).ok_or_else(|| "invalid part path".to_string())?;
    let ranges: Vec<Range<usize>> = document
        .root_element()
        .children()
        .filter(|node| {
            node.is_element()
                && local_name(*node) == "Override"
                && node
                    .attribute("PartName")
                    .and_then(normalize_part_path)
                    .as_deref()
                    == Some(normalized.as_str())
        })
        .map(|node| node.range())
        .collect();
    if ranges.is_empty() {
        return Ok(());
    }
    let mut updated = xml.to_string();
    for range in ranges.into_iter().rev() {
        updated.replace_range(range, "");
    }
    parts.insert(path.to_string(), updated.into_bytes());
    Ok(())
}

fn next_relationship_id(relationships: &HashMap<String, Relationship>) -> String {
    let mut index = 1u64;
    loop {
        let candidate = format!("rId{index}");
        if !relationships.contains_key(&candidate) {
            return candidate;
        }
        index += 1;
    }
}

fn add_relationship(
    parts: &mut BTreeMap<String, Vec<u8>>,
    owner: &str,
    id: &str,
    kind: &str,
    target_part: &str,
) -> Result<(), String> {
    let relationships = parse_relationships(parts, owner)?;
    if relationships.contains_key(id) {
        return Err(format!("relationship {id} already exists on {owner}"));
    }
    let path = relationship_part(owner);
    let target = relative_relationship_target(owner, target_part);
    if !parts.contains_key(&path) {
        parts.insert(
            path,
            format!(
                "<Relationships xmlns=\"{PACKAGE_REL_NS}\"><Relationship Id=\"{}\" Type=\"{}\" Target=\"{}\"/></Relationships>",
                xml_escape_attribute(id),
                xml_escape_attribute(kind),
                xml_escape_attribute(&target)
            )
            .into_bytes(),
        );
        return Ok(());
    }
    let bytes = parts.get(&path).unwrap();
    let xml = std::str::from_utf8(bytes).map_err(|error| format!("{path} UTF-8: {error}"))?;
    let document = Document::parse(xml).map_err(|error| format!("{path} XML: {error}"))?;
    let root = document.root_element();
    if local_name(root) != "Relationships" {
        return Err(format!("{path} root is not Relationships"));
    }
    let prefix = qname_prefix(open_tag_qname(xml, root.range().start)?);
    let qname = qualify(prefix, "Relationship");
    let fragment = format!(
        "<{qname} Id=\"{}\" Type=\"{}\" Target=\"{}\"/>",
        xml_escape_attribute(id),
        xml_escape_attribute(kind),
        xml_escape_attribute(&target)
    );
    let updated = insert_child_before_close(xml, root, &fragment)?;
    parts.insert(path, updated.into_bytes());
    Ok(())
}

fn remove_relationship(
    parts: &mut BTreeMap<String, Vec<u8>>,
    owner: &str,
    id: &str,
) -> Result<(), String> {
    let path = relationship_part(owner);
    let Some(bytes) = parts.get(&path) else {
        return Ok(());
    };
    let xml = std::str::from_utf8(bytes).map_err(|error| format!("{path} UTF-8: {error}"))?;
    let document = Document::parse(xml).map_err(|error| format!("{path} XML: {error}"))?;
    let Some(relationship) = document.descendants().find(|node| {
        node.is_element() && local_name(*node) == "Relationship" && node.attribute("Id") == Some(id)
    }) else {
        return Ok(());
    };
    let updated = remove_range(xml, relationship.range());
    parts.insert(path, updated.into_bytes());
    Ok(())
}

fn add_table_part_reference(
    parts: &mut BTreeMap<String, Vec<u8>>,
    sheet_part: &str,
    relationship_id: &str,
) -> Result<(), String> {
    let bytes = parts
        .get(sheet_part)
        .ok_or_else(|| format!("missing worksheet {sheet_part}"))?;
    let xml = std::str::from_utf8(bytes).map_err(|error| format!("{sheet_part} UTF-8: {error}"))?;
    let document = Document::parse(xml).map_err(|error| format!("{sheet_part} XML: {error}"))?;
    let root = document.root_element();
    if local_name(root) != "worksheet" {
        return Err(format!("{sheet_part} root is not worksheet"));
    }
    let mut output = if root
        .attributes()
        .any(|attribute| attribute.namespace() == Some(REL_NS))
        || xml[root.range().start..scan_open_tag_end(xml, root.range().start)?].contains("xmlns:r=")
    {
        xml.to_string()
    } else {
        patch_open_tag(
            xml,
            root.range().start,
            &[("xmlns:r".to_string(), Some(REL_NS.to_string()))],
        )?
    };
    let document =
        Document::parse(&output).map_err(|error| format!("{sheet_part} XML: {error}"))?;
    let root = document.root_element();
    let prefix = qname_prefix(open_tag_qname(&output, root.range().start)?);
    let table_part_qname = qualify(prefix, "tablePart");
    let fragment = format!(
        "<{table_part_qname} r:id=\"{}\"/>",
        xml_escape_attribute(relationship_id)
    );
    if let Some(container) = direct_child(root, "tableParts") {
        output = insert_child_before_close(&output, container, &fragment)?;
        let document =
            Document::parse(&output).map_err(|error| format!("{sheet_part} XML: {error}"))?;
        let container = direct_child(document.root_element(), "tableParts").unwrap();
        let count = direct_children(container, "tablePart").count();
        output = patch_open_tag(
            &output,
            container.range().start,
            &[("count".to_string(), Some(count.to_string()))],
        )?;
    } else {
        let container_qname = qualify(prefix, "tableParts");
        let table_parts = format!("<{container_qname} count=\"1\">{fragment}</{container_qname}>");
        output = insert_ordered_child(
            &output,
            root,
            "tableParts",
            &table_parts,
            WORKSHEET_CHILD_ORDER,
        )?;
    }
    parts.insert(sheet_part.to_string(), output.into_bytes());
    Ok(())
}

fn remove_table_part_reference(
    parts: &mut BTreeMap<String, Vec<u8>>,
    sheet_part: &str,
    relationship_id_value: &str,
) -> Result<(), String> {
    let bytes = parts
        .get(sheet_part)
        .ok_or_else(|| format!("missing worksheet {sheet_part}"))?;
    let xml = std::str::from_utf8(bytes).map_err(|error| format!("{sheet_part} UTF-8: {error}"))?;
    let document = Document::parse(xml).map_err(|error| format!("{sheet_part} XML: {error}"))?;
    let root = document.root_element();
    let Some(container) = direct_child(root, "tableParts") else {
        return Ok(());
    };
    let Some(table_part) = direct_children(container, "tablePart")
        .find(|node| relationship_id(*node).as_deref() == Some(relationship_id_value))
    else {
        return Ok(());
    };
    let mut output = remove_range(xml, table_part.range());
    let document =
        Document::parse(&output).map_err(|error| format!("{sheet_part} XML: {error}"))?;
    let container = direct_child(document.root_element(), "tableParts").unwrap();
    let count = direct_children(container, "tablePart").count();
    if count == 0 {
        output = remove_range(&output, container.range());
    } else {
        output = patch_open_tag(
            &output,
            container.range().start,
            &[("count".to_string(), Some(count.to_string()))],
        )?;
    }
    parts.insert(sheet_part.to_string(), output.into_bytes());
    Ok(())
}

fn table_names_conflict_with_defined_names(
    parts: &BTreeMap<String, Vec<u8>>,
    workbook_part: &str,
    tables: &[NativeTableModel],
) -> Result<(), String> {
    let (_, workbook) = parse_xml_part(parts, workbook_part)?;
    let defined_names: HashSet<String> = workbook
        .descendants()
        .filter(|node| node.is_element() && local_name(*node) == "definedName")
        .filter_map(|node| node.attribute("name"))
        .map(str::to_ascii_lowercase)
        .collect();
    for table in tables {
        if defined_names.contains(&table.name.to_ascii_lowercase())
            || defined_names.contains(&table.display_name.to_ascii_lowercase())
        {
            return Err(format!(
                "table name {} conflicts with a workbook defined name",
                table.display_name
            ));
        }
    }
    Ok(())
}

fn ranges_overlap(left: &A1Range, right: &A1Range) -> bool {
    left.start_col <= right.end_col
        && right.start_col <= left.end_col
        && left.start_row <= right.end_row
        && right.start_row <= left.end_row
}

fn validate_workbook_tables(
    parts: &BTreeMap<String, Vec<u8>>,
    model: &NativeTableWorkbookModel,
) -> Result<(), String> {
    let mut ids = HashSet::new();
    let mut names = HashSet::new();
    for table in &model.tables {
        if table.id == 0 || !ids.insert(table.id) {
            return Err(format!("duplicate/invalid table id {}", table.id));
        }
        validate_table_name(&table.name)?;
        validate_table_name(&table.display_name)?;
        if !names.insert(table.name.to_ascii_lowercase())
            || (!table.display_name.eq_ignore_ascii_case(&table.name)
                && !names.insert(table.display_name.to_ascii_lowercase()))
        {
            return Err(format!("duplicate table name {}", table.display_name));
        }
    }
    for (index, left) in model.tables.iter().enumerate() {
        let left_range = validate_reference(&left.reference, "table ref")?;
        for right in model.tables.iter().skip(index + 1) {
            if left.sheet_part == right.sheet_part {
                let right_range = validate_reference(&right.reference, "table ref")?;
                if ranges_overlap(&left_range, &right_range) {
                    return Err(format!(
                        "tables {} and {} overlap on {}",
                        left.display_name, right.display_name, left.sheet
                    ));
                }
            }
        }
    }
    table_names_conflict_with_defined_names(parts, &model.workbook_part, &model.tables)
}

fn create_column_payload(value: &Value, index: usize) -> Result<Map<String, Value>, String> {
    match value {
        Value::String(name) => {
            let mut object = Map::new();
            object.insert("id".to_string(), Value::from((index + 1) as u64));
            object.insert("name".to_string(), Value::String(name.clone()));
            Ok(object)
        }
        Value::Object(object) => {
            let mut object = object.clone();
            object
                .entry("id".to_string())
                .or_insert_with(|| Value::from((index + 1) as u64));
            Ok(object)
        }
        _ => Err("table columns must be strings or objects".to_string()),
    }
}

fn create_table_xml(
    id: u64,
    name: &str,
    display_name: &str,
    reference: &str,
    payload: &Map<String, Value>,
) -> Result<String, String> {
    validate_table_name(name)?;
    validate_table_name(display_name)?;
    let columns = payload
        .get("columns")
        .and_then(Value::as_array)
        .ok_or_else(|| "create table requires columns array".to_string())?;
    if columns.is_empty() {
        return Err("table must contain at least one column".to_string());
    }
    let header_row_count = payload
        .get("headerRowCount")
        .and_then(Value::as_u64)
        .unwrap_or(1);
    let totals_row_count = payload
        .get("totalsRowCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    validate_table_shape(reference, header_row_count, totals_row_count, columns.len())?;
    let mut root_attrs = Map::new();
    root_attrs.insert("id".to_string(), Value::from(id));
    root_attrs.insert("name".to_string(), Value::String(name.to_string()));
    root_attrs.insert(
        "displayName".to_string(),
        Value::String(display_name.to_string()),
    );
    root_attrs.insert("ref".to_string(), Value::String(reference.to_string()));
    if header_row_count != 1 || payload.contains_key("headerRowCount") {
        root_attrs.insert("headerRowCount".to_string(), Value::from(header_row_count));
    }
    if totals_row_count != 0 || payload.contains_key("totalsRowCount") {
        root_attrs.insert("totalsRowCount".to_string(), Value::from(totals_row_count));
    }
    for key in [
        "totalsRowShown",
        "insertRow",
        "insertRowShift",
        "published",
        "comment",
        "tableType",
        "connectionId",
        "headerRowDxfId",
        "dataDxfId",
        "totalsRowDxfId",
        "headerRowBorderDxfId",
        "tableBorderDxfId",
        "totalsRowBorderDxfId",
        "headerRowCellStyle",
        "dataCellStyle",
        "totalsRowCellStyle",
    ] {
        if let Some(value) = payload.get(key) {
            root_attrs.insert(key.to_string(), value.clone());
        }
    }
    if let Some(attributes) = payload.get("attributes") {
        root_attrs.insert("attributes".to_string(), attributes.clone());
    }
    let root_attributes = map_attribute_fragment(
        &root_attrs,
        &[
            "id",
            "name",
            "displayName",
            "ref",
            "headerRowCount",
            "totalsRowCount",
            "totalsRowShown",
            "insertRow",
            "insertRowShift",
            "published",
            "comment",
            "tableType",
            "connectionId",
            "headerRowDxfId",
            "dataDxfId",
            "totalsRowDxfId",
            "headerRowBorderDxfId",
            "tableBorderDxfId",
            "totalsRowBorderDxfId",
            "headerRowCellStyle",
            "dataCellStyle",
            "totalsRowCellStyle",
        ],
        &[],
    )?;
    let mut children = String::new();
    match payload.get("autoFilter") {
        Some(Value::Bool(false) | Value::Null) => {}
        Some(Value::Object(filter)) => {
            let mut filter = filter.clone();
            filter.entry("ref".to_string()).or_insert_with(|| {
                Value::String(
                    table_filter_reference(reference, totals_row_count)
                        .unwrap_or_else(|| reference.to_string()),
                )
            });
            children.push_str(&auto_filter_fragment("", &filter)?);
        }
        Some(_) => {
            return Err("create autoFilter must be false, null, or an object".to_string());
        }
        None => {
            let filter_ref = table_filter_reference(reference, totals_row_count)
                .ok_or_else(|| "table has no filterable range".to_string())?;
            children.push_str(&format!(
                "<autoFilter ref=\"{}\"/>",
                xml_escape_attribute(&filter_ref)
            ));
        }
    }
    if let Some(sort) = payload.get("sortState") {
        if !sort.is_null() {
            children.push_str(&sort_state_fragment(
                "",
                sort.as_object()
                    .ok_or_else(|| "create sortState must be an object or null".to_string())?,
            )?);
        }
    }
    let column_fragments: String = columns
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let payload = create_column_payload(value, index)?;
            table_column_fragment("", &payload)
        })
        .collect::<Result<Vec<_>, String>>()?
        .concat();
    children.push_str(&format!(
        "<tableColumns count=\"{}\">{column_fragments}</tableColumns>",
        columns.len()
    ));
    match payload.get("styleInfo") {
        Some(Value::Null | Value::Bool(false)) => {}
        Some(Value::Object(style)) => children.push_str(&table_style_fragment("", style)?),
        Some(_) => return Err("styleInfo must be an object, false, or null".to_string()),
        None => children.push_str(
            "<tableStyleInfo name=\"TableStyleMedium2\" showFirstColumn=\"0\" showLastColumn=\"0\" showRowStripes=\"1\" showColumnStripes=\"0\"/>",
        ),
    }
    let xml = format!("<table xmlns=\"{SPREADSHEET_NS}\"{root_attributes}>{children}</table>");
    validate_table_xml(&xml, true)?;
    Ok(xml)
}

fn resolve_sheet_for_create<'a>(
    model: &'a NativeTableWorkbookModel,
    payload: &Map<String, Value>,
) -> Result<&'a NativeWorksheetFilterSortModel, String> {
    let sheet_part = payload.get("sheetPart").and_then(Value::as_str);
    let sheet_name = payload.get("sheet").and_then(Value::as_str);
    let sheet_id = payload.get("sheetId").and_then(Value::as_u64);
    let matches: Vec<&NativeWorksheetFilterSortModel> = model
        .worksheets
        .iter()
        .filter(|sheet| {
            sheet_part.is_none_or(|value| sheet.sheet_part == value)
                && sheet_name.is_none_or(|value| sheet.sheet == value)
                && sheet_id.is_none_or(|value| sheet.sheet_id == value)
        })
        .collect();
    match matches.as_slice() {
        [sheet] => Ok(*sheet),
        [] => Err("create table sheet selector did not match".to_string()),
        _ => Err("create table sheet selector is ambiguous".to_string()),
    }
}

fn next_table_part(parts: &BTreeMap<String, Vec<u8>>, workbook_part: &str, id: u64) -> String {
    let directory = part_directory(workbook_part);
    let base = if directory.is_empty() {
        "tables".to_string()
    } else {
        format!("{directory}/tables")
    };
    let mut suffix = id;
    loop {
        let candidate = format!("{base}/table-{suffix}.xml");
        if !parts.contains_key(&candidate) {
            return candidate;
        }
        suffix += 1;
    }
}

fn embedded_patch<'a>(
    object: &'a Map<String, Value>,
    key: &str,
) -> Result<&'a Map<String, Value>, String> {
    object
        .get(key)
        .map(|value| {
            value
                .as_object()
                .ok_or_else(|| format!("{key} must be an object"))
        })
        .unwrap_or(Ok(object))
}

fn package_entries<'a>(
    patch: &'a Map<String, Value>,
    key: &str,
) -> Result<Vec<&'a Map<String, Value>>, String> {
    object_operations(patch, key)
}

fn table_column_name_map(xml: &str) -> Result<BTreeMap<u64, String>, String> {
    let document = Document::parse(xml).map_err(|error| format!("Table XML: {error}"))?;
    let root = document.root_element();
    let mut result = BTreeMap::new();
    if let Some(container) = direct_child(root, "tableColumns") {
        for column in direct_children(container, "tableColumn") {
            if let (Some(id), Some(name)) =
                (parse_u64(column.attribute("id")), column.attribute("name"))
            {
                result.insert(id, name.to_string());
            }
        }
    }
    Ok(result)
}

fn table_column_records(xml: &str) -> Result<Vec<(u64, String)>, String> {
    let document = Document::parse(xml).map_err(|error| format!("Table XML: {error}"))?;
    let root = document.root_element();
    Ok(direct_child(root, "tableColumns")
        .into_iter()
        .flat_map(|container| direct_children(container, "tableColumn"))
        .filter_map(|column| {
            Some((
                parse_u64(column.attribute("id"))?,
                column.attribute("name")?.to_string(),
            ))
        })
        .collect())
}

fn table_root_identity(xml: &str) -> Result<(String, String), String> {
    let document = Document::parse(xml).map_err(|error| format!("Table XML: {error}"))?;
    let root = document.root_element();
    Ok((
        root.attribute("name").unwrap_or("").to_string(),
        root.attribute("displayName")
            .or_else(|| root.attribute("name"))
            .unwrap_or("")
            .to_string(),
    ))
}

fn bool_option(entry: &Map<String, Value>, package: &Map<String, Value>, key: &str) -> bool {
    entry
        .get(key)
        .and_then(Value::as_bool)
        .or_else(|| package.get(key).and_then(Value::as_bool))
        .unwrap_or(false)
}

/// Atomically edits/creates/deletes native tables and ordinary worksheet filters/sorts.
///
/// Package patch shape:
/// `{ "tableEdits": [{"part":"...", "patch":{...}}],
///    "worksheetEdits": [{"part":"...", "patch":{...}}],
///    "createTables": [{"sheetPart":"...", "name":"Table1", "ref":"A1:C9",
///                       "columns":["A","B","C"]}],
///    "deleteTables": [{"part":"..."}] }`.
///
/// Renaming a table/column referenced by formulas is rejected unless
/// `rewriteStructuredReferences:true`; then only formula text nodes in the resolved table context
/// are rewritten.  Deleting referenced columns/tables requires an explicit allow flag, and table
/// slicer dependencies are never silently orphaned.  The caller receives no partial mutation.
pub(crate) fn apply_table_package_edit(
    parts: &mut BTreeMap<String, Vec<u8>>,
    patch: &Value,
) -> Result<NativeTableWorkbookModel, String> {
    let package = patch
        .as_object()
        .ok_or_else(|| "table package patch must be an object".to_string())?;
    let original_model = inspect_native_tables(parts)?;
    if package_entries(package, "tableEdits")?.is_empty()
        && package_entries(package, "worksheetEdits")?.is_empty()
        && package_entries(package, "createTables")?.is_empty()
        && package_entries(package, "deleteTables")?.is_empty()
    {
        return Ok(original_model);
    }
    let mut output = parts.clone();

    for edit in package_entries(package, "worksheetEdits")? {
        let part = edit
            .get("part")
            .or_else(|| edit.get("sheetPart"))
            .and_then(Value::as_str)
            .ok_or_else(|| "worksheet edit requires part".to_string())?;
        if !original_model
            .worksheets
            .iter()
            .any(|sheet| sheet.sheet_part == part)
        {
            return Err(format!("{part} is not a known worksheet"));
        }
        let worksheet_patch = embedded_patch(edit, "patch")?;
        let xml = std::str::from_utf8(
            output
                .get(part)
                .ok_or_else(|| format!("missing worksheet {part}"))?,
        )
        .map_err(|error| format!("{part} UTF-8: {error}"))?;
        let updated =
            apply_worksheet_filter_sort_edit(xml, &Value::Object(worksheet_patch.clone()))?;
        output.insert(part.to_string(), updated.into_bytes());
    }

    for edit in package_entries(package, "tableEdits")? {
        let part = edit
            .get("part")
            .and_then(Value::as_str)
            .ok_or_else(|| "table edit requires part".to_string())?;
        let table = original_model
            .tables
            .iter()
            .find(|table| table.part == part)
            .ok_or_else(|| format!("{part} is not a known table part"))?;
        let table_patch = embedded_patch(edit, "patch")?;
        let old_xml = std::str::from_utf8(
            output
                .get(part)
                .ok_or_else(|| format!("missing table part {part}"))?,
        )
        .map_err(|error| format!("{part} UTF-8: {error}"))?;
        let old_column_records = table_column_records(old_xml)?;
        let updated = apply_table_part_edit(old_xml, &Value::Object(table_patch.clone()))?;
        let new_columns = table_column_name_map(&updated)?;
        let new_column_records = table_column_records(&updated)?;
        let (_new_name, new_display_name) = table_root_identity(&updated)?;
        output.insert(part.to_string(), updated.into_bytes());

        // Structured references are keyed by displayName. The legacy `name` attribute may differ
        // in third-party files and changing it alone must not rewrite formulas.
        let table_renamed = !new_display_name.eq_ignore_ascii_case(&table.display_name);
        let old_ids: HashSet<u64> = old_column_records.iter().map(|(id, _)| *id).collect();
        let mut column_renames = Vec::new();
        let mut deleted_columns = Vec::new();
        for (index, (old_id, old_name)) in old_column_records.iter().enumerate() {
            if let Some(new_name) = new_columns.get(old_id) {
                if !new_name.eq_ignore_ascii_case(old_name) {
                    column_renames.push((old_name.clone(), new_name.clone()));
                }
                continue;
            }
            // An id-only metadata correction is not a semantic deletion. Match the unchanged
            // unique column name first, then a newly assigned id in the same logical position.
            if new_column_records
                .iter()
                .any(|(_, new_name)| new_name.eq_ignore_ascii_case(old_name))
            {
                continue;
            }
            if let Some((new_id, new_name)) = new_column_records.get(index) {
                if !old_ids.contains(new_id) {
                    column_renames.push((old_name.clone(), new_name.clone()));
                    continue;
                }
            }
            deleted_columns.push(old_name.clone());
        }
        let hits = collect_formula_hits(&output);
        if table_renamed {
            let references = table_reference_hits(&hits, table);
            if !references.is_empty() && !bool_option(edit, package, "rewriteStructuredReferences")
            {
                return Err(format!(
                    "table {} has {} structured-reference formulas; set rewriteStructuredReferences",
                    table.display_name,
                    references.len()
                ));
            }
        }
        for (old, _) in &column_renames {
            let references = column_reference_hits(&hits, table, old);
            if !references.is_empty() && !bool_option(edit, package, "rewriteStructuredReferences")
            {
                return Err(format!(
                    "table column {} has {} structured-reference formulas; set rewriteStructuredReferences",
                    old,
                    references.len()
                ));
            }
        }
        for deleted in &deleted_columns {
            let references = column_reference_hits(&hits, table, deleted);
            if !references.is_empty() && !bool_option(edit, package, "allowDeleteReferencedColumns")
            {
                return Err(format!(
                    "cannot delete referenced table column {deleted} ({} formulas)",
                    references.len()
                ));
            }
        }
        if bool_option(edit, package, "rewriteStructuredReferences")
            && (table_renamed || !column_renames.is_empty())
        {
            rewrite_structured_references(
                &mut output,
                table,
                table_renamed.then_some(new_display_name.as_str()),
                &column_renames,
            )?;
        }
    }

    for deletion in package_entries(package, "deleteTables")? {
        let part = deletion
            .get("part")
            .and_then(Value::as_str)
            .ok_or_else(|| "delete table requires part".to_string())?;
        let current_model = inspect_native_tables(&output)?;
        let table = current_model
            .tables
            .iter()
            .find(|table| table.part == part)
            .ok_or_else(|| format!("{part} is not a known table part"))?;
        let formula_hits = collect_formula_hits(&output);
        let references: Vec<&FormulaHit> = table_reference_hits(&formula_hits, table)
            .into_iter()
            .filter(|hit| hit.part != table.part)
            .collect();
        if !references.is_empty() && !bool_option(deletion, package, "allowDeleteReferencedTable") {
            return Err(format!(
                "cannot delete table {} referenced by {} formulas",
                table.display_name,
                references.len()
            ));
        }
        let slicers = table_slicer_references(&output, table.id);
        if !slicers.is_empty() {
            return Err(format!(
                "cannot delete table {} while slicer caches reference it: {}",
                table.display_name,
                slicers.join(", ")
            ));
        }
        let relationship_id = table
            .relationship_id
            .as_deref()
            .ok_or_else(|| format!("{} has no worksheet relationship id", table.part))?;
        remove_table_part_reference(&mut output, &table.sheet_part, relationship_id)?;
        remove_relationship(&mut output, &table.sheet_part, relationship_id)?;
        remove_content_type_override(&mut output, &table.part)?;
        output.remove(&table.part);
        output.remove(&relationship_part(&table.part));
    }

    for creation in package_entries(package, "createTables")? {
        let current_model = inspect_native_tables(&output)?;
        let sheet = resolve_sheet_for_create(&current_model, creation)?.clone();
        let id = creation
            .get("id")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| {
                current_model
                    .tables
                    .iter()
                    .map(|table| table.id)
                    .max()
                    .unwrap_or(0)
                    + 1
            });
        if id == 0 || current_model.tables.iter().any(|table| table.id == id) {
            return Err(format!("table id {id} is invalid or already used"));
        }
        let name = creation
            .get("name")
            .or_else(|| creation.get("displayName"))
            .and_then(Value::as_str)
            .ok_or_else(|| "create table requires name/displayName".to_string())?;
        let display_name = creation
            .get("displayName")
            .and_then(Value::as_str)
            .unwrap_or(name);
        if current_model.tables.iter().any(|table| {
            table.name.eq_ignore_ascii_case(name)
                || table.display_name.eq_ignore_ascii_case(name)
                || table.name.eq_ignore_ascii_case(display_name)
                || table.display_name.eq_ignore_ascii_case(display_name)
        }) {
            return Err(format!("table name {display_name} is already used"));
        }
        let reference = creation
            .get("ref")
            .and_then(Value::as_str)
            .ok_or_else(|| "create table requires ref".to_string())?;
        let part = creation
            .get("part")
            .and_then(Value::as_str)
            .map(|part| {
                normalize_part_path(part)
                    .filter(|part| part.ends_with(".xml"))
                    .ok_or_else(|| "create table part must be a normalized .xml path".to_string())
            })
            .transpose()?
            .unwrap_or_else(|| next_table_part(&output, &current_model.workbook_part, id));
        if output.contains_key(&part) {
            return Err(format!("OPC part {part} already exists"));
        }
        let relationships = parse_relationships(&output, &sheet.sheet_part)?;
        let relationship_id = creation
            .get("relationshipId")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| next_relationship_id(&relationships));
        if relationships.contains_key(&relationship_id) {
            return Err(format!(
                "relationship {relationship_id} already exists on {}",
                sheet.sheet_part
            ));
        }
        let xml = create_table_xml(id, name, display_name, reference, creation)?;
        output.insert(part.clone(), xml.into_bytes());
        add_relationship(
            &mut output,
            &sheet.sheet_part,
            &relationship_id,
            TABLE_RELATIONSHIP_TYPE,
            &part,
        )?;
        add_table_part_reference(&mut output, &sheet.sheet_part, &relationship_id)?;
        ensure_content_type_override(&mut output, &part, TABLE_CONTENT_TYPE)?;
    }

    let final_model = inspect_native_tables(&output)?;
    validate_workbook_tables(&output, &final_model)?;
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
                    r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Override PartName="/odd/book.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/odd/pages/data.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/odd/native/list-alpha.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.table+xml"/></Types>"#,
                ),
            ),
            (
                "_rels/.rels".to_string(),
                bytes(
                    r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="office-x" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="odd/book.xml"/></Relationships>"#,
                ),
            ),
            (
                "odd/book.xml".to_string(),
                bytes(
                    r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Data" sheetId="7" r:id="sheet-weird"/></sheets><definedNames><definedName name="_Print_Area">Data!$A$1:$G$9</definedName></definedNames></workbook>"#,
                ),
            ),
            (
                "odd/_rels/book.xml.rels".to_string(),
                bytes(
                    r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="sheet-weird" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="pages/data.xml"/></Relationships>"#,
                ),
            ),
            (
                "odd/pages/data.xml".to_string(),
                bytes(
                    r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:v="urn:vendor" v:keep="sheet"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Item</t></is></c><c r="B1" t="inlineStr"><is><t>Amount</t></is></c></row><row r="2"><c r="A2"><f>[@Amount]*2</f><v>20</v></c><c r="C2"><f>SalesTbl[Amount]+1</f><v>11</v></c></row></sheetData><autoFilter ref="F1:G9" v:keep="filter"><filterColumn colId="0"><filters blank="1"><filter val="x"/><extLst><ext uri="vendor-filter"/></extLst></filters></filterColumn><sortState ref="F2:G9"><sortCondition ref="F2:F9"/></sortState></autoFilter><tableParts count="1" v:keep="parts"><tablePart r:id="table-weird"/></tableParts><extLst><ext uri="vendor-sheet"/></extLst></worksheet>"#,
                ),
            ),
            (
                "odd/pages/_rels/data.xml.rels".to_string(),
                bytes(
                    r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="table-weird" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/table" Target="../native/list-alpha.xml"/><Relationship Id="keep-me" Type="urn:vendor/relationship" Target="../vendor/payload.xml"/></Relationships>"#,
                ),
            ),
            (
                "odd/native/list-alpha.xml".to_string(),
                bytes(
                    r#"<table xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:v="urn:vendor" id="42" name="SalesTbl" displayName="SalesTbl" ref="A1:B4" v:keep="root"><autoFilter ref="A1:B4"><filterColumn colId="1" v:keep="column-filter"><filters blank="0"><filter val="10"/><dateGroupItem year="2026" month="8" dateTimeGrouping="month"/><extLst><ext uri="vendor-definition"/></extLst></filters><extLst><ext uri="vendor-column"/></extLst></filterColumn><sortState ref="A2:B4" caseSensitive="1"><sortCondition ref="B2:B4" descending="1" customList="10,20"/><extLst><ext uri="vendor-sort"/></extLst></sortState></autoFilter><tableColumns count="2" v:keep="columns"><tableColumn id="7" name="Item"/><tableColumn id="9" name="Amount" totalsRowFunction="sum" v:keep="amount"><calculatedColumnFormula>=[@Amount]*2</calculatedColumnFormula><extLst><ext uri="vendor-column-meta"/></extLst></tableColumn></tableColumns><tableStyleInfo name="TableStyleMedium4" showRowStripes="1" v:keep="style"/><extLst><ext uri="vendor-table"/></extLst></table>"#,
                ),
            ),
            (
                "odd/native/_rels/list-alpha.xml.rels".to_string(),
                bytes(
                    r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="query-keep" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/queryTable" Target="../query/q.xml"/></Relationships>"#,
                ),
            ),
            (
                "odd/vendor/payload.xml".to_string(),
                bytes(r#"<vendor xmlns="urn:vendor">preserve</vendor>"#),
            ),
        ])
    }

    #[test]
    fn resolves_nonstandard_table_graph_and_all_native_state() {
        let model = inspect_native_tables(&fixture()).unwrap();
        assert_eq!(model.workbook_part, "odd/book.xml");
        assert_eq!(model.tables.len(), 1);
        let table = &model.tables[0];
        assert_eq!(table.part, "odd/native/list-alpha.xml");
        assert_eq!(table.relationship_id.as_deref(), Some("table-weird"));
        assert_eq!(table.sheet, "Data");
        assert_eq!(table.id, 42);
        assert_eq!(
            table.columns[1].calculated_column_formula.as_deref(),
            Some("=[@Amount]*2")
        );
        let filter = table.auto_filter.as_ref().unwrap();
        assert_eq!(filter.filter_columns[0].column_id, 1);
        assert_eq!(
            filter.filter_columns[0].definition.as_ref().unwrap().kind,
            "filters"
        );
        assert_eq!(filter.sort_state.as_ref().unwrap().conditions.len(), 1);
        assert_eq!(
            model.worksheets[0]
                .auto_filter
                .as_ref()
                .unwrap()
                .reference
                .as_deref(),
            Some("F1:G9")
        );
        assert!(model.warnings.is_empty(), "{:?}", model.warnings);
    }

    #[test]
    fn no_op_edits_are_byte_exact() {
        let parts = fixture();
        let table = std::str::from_utf8(parts.get("odd/native/list-alpha.xml").unwrap()).unwrap();
        let sheet = std::str::from_utf8(parts.get("odd/pages/data.xml").unwrap()).unwrap();
        assert_eq!(apply_table_part_edit(table, &json!({})).unwrap(), table);
        assert_eq!(
            apply_worksheet_filter_sort_edit(sheet, &json!({})).unwrap(),
            sheet
        );
    }

    #[test]
    fn table_edit_resizes_columns_filters_and_style_losslessly() {
        let parts = fixture();
        let original =
            std::str::from_utf8(parts.get("odd/native/list-alpha.xml").unwrap()).unwrap();
        let updated = apply_table_part_edit(
            original,
            &json!({
                "ref": "A1:C4",
                "columnOperations": [
                    {"op":"add", "patch":{"id":11, "name":"Region", "dataDxfId":3}}
                ],
                "autoFilter": {
                    "filterColumnOperations": [
                        {"op":"add", "patch":{"colId":2, "customFilters":{"and":true, "conditions":[{"operator":"notEqual", "val":"EU"}, {"operator":"notEqual", "val":"APAC"}]}}}
                    ]
                },
                "styleInfo": {"showLastColumn":true, "showColumnStripes":true}
            }),
        )
        .unwrap();
        assert!(updated.contains("ref=\"A1:C4\""));
        assert!(updated.contains("count=\"3\""));
        assert!(updated.contains("name=\"Region\""));
        assert!(updated.contains("<customFilters and=\"1\">"));
        assert!(updated.contains("showLastColumn=\"1\""));
        assert!(updated.contains("v:keep=\"root\""));
        assert!(updated.contains("vendor-definition"));
        assert!(updated.contains("vendor-table"));
        validate_table_xml(&updated, true).unwrap();
    }

    #[test]
    fn identity_attribute_updates_keep_targeting_the_same_native_children() {
        let parts = fixture();
        let original =
            std::str::from_utf8(parts.get("odd/native/list-alpha.xml").unwrap()).unwrap();
        let updated = apply_table_part_edit(
            original,
            &json!({
                "columnOperations":[{
                    "op":"update", "id":9,
                    "patch":{"id":10, "calculatedColumnFormula":"=[@Amount]+5"}
                }],
                "autoFilter":{"filterColumnOperations":[{
                    "op":"update", "colId":1,
                    "patch":{"colId":0, "filters":{"values":["20"]}}
                }]}
            }),
        )
        .unwrap();
        assert!(updated.contains("id=\"10\" name=\"Amount\""));
        assert!(updated.contains(">=[@Amount]+5</calculatedColumnFormula>"));
        assert!(updated.contains("filterColumn colId=\"0\""));
        assert!(updated.contains("vendor-definition"));

        let mut package = fixture();
        apply_table_package_edit(
            &mut package,
            &json!({
                "tableEdits":[{
                    "part":"odd/native/list-alpha.xml",
                    "patch":{"columnOperations":[{"op":"update","id":9,"patch":{"id":10}}]}
                }]
            }),
        )
        .unwrap();
    }

    #[test]
    fn worksheet_filter_supports_every_standard_definition_and_multisort() {
        let parts = fixture();
        let sheet = std::str::from_utf8(parts.get("odd/pages/data.xml").unwrap()).unwrap();
        let updated = apply_worksheet_filter_sort_edit(
            sheet,
            &json!({
                "autoFilter": {
                    "ref":"F1:K9",
                    "filterColumns":[
                        {"colId":0,"filters":{"blank":true,"values":["x","y"],"dateGroups":[{"year":2026,"month":8,"dateTimeGrouping":"month"}]}},
                        {"colId":1,"customFilters":{"and":true,"conditions":[{"operator":"greaterThan","val":"10"}]}},
                        {"colId":2,"dynamicFilter":{"type":"thisMonth","val":"1"}},
                        {"colId":3,"top10":{"top":true,"percent":false,"val":5}},
                        {"colId":4,"colorFilter":{"dxfId":2,"cellColor":true}},
                        {"colId":5,"iconFilter":{"iconSet":"3Arrows","iconId":1}}
                    ],
                    "sortState":{"ref":"F2:K9","caseSensitive":true,"conditions":[{"ref":"G2:G9","descending":true},{"ref":"F2:F9","sortBy":"cellColor","dxfId":2}]}
                },
                "sortState":{"ref":"F2:K9","columnSort":true,"conditions":[{"ref":"F2:K2"}]}
            }),
        )
        .unwrap();
        assert!(updated.contains("vendor-sheet"));
        assert!(updated.contains("v:keep=\"filter\""));
        let document = Document::parse(&updated).unwrap();
        let root = document.root_element();
        let auto = parse_auto_filter(direct_child(root, "autoFilter").unwrap());
        let kinds: Vec<String> = auto
            .filter_columns
            .iter()
            .map(|column| column.definition.as_ref().unwrap().kind.clone())
            .collect();
        assert_eq!(
            kinds,
            vec![
                "filters",
                "customFilters",
                "dynamicFilter",
                "top10",
                "colorFilter",
                "iconFilter"
            ]
        );
        assert_eq!(auto.sort_state.unwrap().conditions.len(), 2);
        assert!(parse_sort_state(direct_child(root, "sortState").unwrap()).column_sort);
    }

    #[test]
    fn package_rename_requires_opt_in_and_rewrites_only_formula_text() {
        let mut parts = fixture();
        let before = parts.clone();
        let patch = json!({
            "tableEdits":[{
                "part":"odd/native/list-alpha.xml",
                "patch":{
                    "name":"RenamedTbl",
                    "displayName":"RenamedTbl",
                    "columnOperations":[{"op":"update","id":9,"patch":{"name":"Net"}}]
                }
            }]
        });
        let error = apply_table_package_edit(&mut parts, &patch).unwrap_err();
        assert!(error.contains("rewriteStructuredReferences"));
        assert_eq!(parts, before);

        let model = apply_table_package_edit(
            &mut parts,
            &json!({
                "rewriteStructuredReferences":true,
                "tableEdits":[{
                    "part":"odd/native/list-alpha.xml",
                    "patch":{
                        "name":"RenamedTbl",
                        "displayName":"RenamedTbl",
                        "columnOperations":[{"op":"update","id":9,"patch":{"name":"Net"}}]
                    }
                }]
            }),
        )
        .unwrap();
        assert_eq!(model.tables[0].display_name, "RenamedTbl");
        let sheet = std::str::from_utf8(parts.get("odd/pages/data.xml").unwrap()).unwrap();
        let table = std::str::from_utf8(parts.get("odd/native/list-alpha.xml").unwrap()).unwrap();
        assert!(sheet.contains("RenamedTbl[Net]+1"));
        assert!(sheet.contains("[@Net]*2"));
        assert!(table.contains("=[@Net]*2"));
        assert!(table.contains("vendor-column-meta"));
    }

    #[test]
    fn create_and_delete_table_updates_all_opc_edges_atomically() {
        let mut parts = fixture();
        let created = apply_table_package_edit(
            &mut parts,
            &json!({
                "createTables":[{
                    "sheetPart":"odd/pages/data.xml",
                    "name":"SecondTbl",
                    "ref":"D1:E4",
                    "columns":["Code", {"name":"Value", "totalsRowFunction":"sum"}],
                    "styleInfo":{"name":"TableStyleLight9", "showRowStripes":true}
                }]
            }),
        )
        .unwrap();
        assert_eq!(created.tables.len(), 2);
        let second = created
            .tables
            .iter()
            .find(|table| table.display_name == "SecondTbl")
            .unwrap()
            .clone();
        assert!(parts.contains_key(&second.part));
        let sheet = std::str::from_utf8(parts.get("odd/pages/data.xml").unwrap()).unwrap();
        assert!(sheet.contains(&format!(
            "r:id=\"{}\"",
            second.relationship_id.as_deref().unwrap()
        )));
        let rels =
            std::str::from_utf8(parts.get("odd/pages/_rels/data.xml.rels").unwrap()).unwrap();
        assert!(rels.contains(&relative_relationship_target(
            "odd/pages/data.xml",
            &second.part
        )));
        let types = std::str::from_utf8(parts.get("[Content_Types].xml").unwrap()).unwrap();
        assert!(types.contains(&format!("PartName=\"/{}\"", second.part)));

        let deleted =
            apply_table_package_edit(&mut parts, &json!({"deleteTables":[{"part":second.part}]}))
                .unwrap();
        assert_eq!(deleted.tables.len(), 1);
        assert!(!parts.contains_key(&second.part));
        let rels =
            std::str::from_utf8(parts.get("odd/pages/_rels/data.xml.rels").unwrap()).unwrap();
        assert!(rels.contains("keep-me"));
        assert!(!rels.contains(second.relationship_id.as_deref().unwrap()));
    }

    #[test]
    fn deleting_referenced_table_or_column_is_safe_by_default() {
        let mut parts = fixture();
        let before = parts.clone();
        let error = apply_table_package_edit(
            &mut parts,
            &json!({"deleteTables":[{"part":"odd/native/list-alpha.xml"}]}),
        )
        .unwrap_err();
        assert!(error.contains("referenced"));
        assert_eq!(parts, before);

        let error = apply_table_package_edit(
            &mut parts,
            &json!({
                "tableEdits":[{
                    "part":"odd/native/list-alpha.xml",
                    "patch":{"ref":"A1:A4", "autoFilter":null, "columnOperations":[{"op":"delete","id":9}]}
                }]
            }),
        )
        .unwrap_err();
        assert!(error.contains("referenced table column"));
        assert_eq!(parts, before);
    }

    #[test]
    fn invalid_structural_edit_never_leaks_partial_package_state() {
        let mut parts = fixture();
        let before = parts.clone();
        let error = apply_table_package_edit(
            &mut parts,
            &json!({
                "worksheetEdits":[{"part":"odd/pages/data.xml","patch":{"sortState":null}}],
                "tableEdits":[{"part":"odd/native/list-alpha.xml","patch":{"ref":"A1:C4"}}]
            }),
        )
        .unwrap_err();
        assert!(error.contains("range width"));
        assert_eq!(parts, before);
    }
}
