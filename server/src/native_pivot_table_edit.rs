//! Lossless reader/editor for native OOXML PivotTable definitions.
//!
//! A PivotTable definition is an extensible OOXML document.  Excel and third-party producers
//! routinely add attributes, namespace-qualified children, and `extLst` payloads which are not
//! represented by the public editor model below.  Consequently this module never serialises a
//! complete `pivotTableDefinition`.  It resolves the native OPC relationship graph for reading
//! and applies differential edits to the existing XML ranges.  Unedited nodes, unknown
//! attributes/children, namespace declarations, whitespace, and extensions remain byte exact.

use roxmltree::{Document, Node};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ops::Range;

const REL_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

const ROOT_BOOLEAN_ATTRIBUTES: &[&str] = &[
    "applyAlignmentFormats",
    "applyBorderFormats",
    "applyFontFormats",
    "applyNumberFormats",
    "applyPatternFormats",
    "applyWidthHeightFormats",
    "asteriskTotals",
    "colGrandTotals",
    "compact",
    "compactData",
    "customListSort",
    "dataOnRows",
    "disableFieldList",
    "editData",
    "enableDrill",
    "enableFieldProperties",
    "enableWizard",
    "fieldListSortAscending",
    "fieldPrintTitles",
    "gridDropZones",
    "immersive",
    "itemPrintTitles",
    "mdxSubqueries",
    "mergeItem",
    "multipleFieldFilters",
    "outline",
    "outlineData",
    "pageOverThenDown",
    "preserveFormatting",
    "printDrill",
    "published",
    "rowGrandTotals",
    "showCalcMbrs",
    "showDataDropDown",
    "showDataTips",
    "showDrill",
    "showDropZones",
    "showEmptyCol",
    "showEmptyRow",
    "showError",
    "showHeaders",
    "showItems",
    "showMemberPropertyTips",
    "showMissing",
    "showMultipleLabel",
    "showValuesRow",
    "subtotalHiddenItems",
    "useAutoFormatting",
    "vacatedStyle",
    "visualTotals",
];

const ROOT_UNSIGNED_ATTRIBUTES: &[&str] = &[
    "autoFormatId",
    "chartFormat",
    "dataPosition",
    "indent",
    "pageWrap",
    "updatedVersion",
    "createdVersion",
    "minRefreshableVersion",
];

const ROOT_STRING_ATTRIBUTES: &[&str] = &[
    "dataCaption",
    "errorCaption",
    "grandTotalCaption",
    "missingCaption",
    "name",
    "pageStyle",
    "pivotTableStyle",
    "tag",
    "title",
    "colHeaderCaption",
    "rowHeaderCaption",
];

const LOCATION_UNSIGNED_ATTRIBUTES: &[&str] = &[
    "firstHeaderRow",
    "firstDataRow",
    "firstDataCol",
    "rowPageCount",
    "colPageCount",
];

const PIVOT_FIELD_BOOLEAN_ATTRIBUTES: &[&str] = &[
    "allDrilled",
    "autoShow",
    "avgSubtotal",
    "compact",
    "countASubtotal",
    "countSubtotal",
    "dataField",
    "dataSourceSort",
    "defaultAttributeDrillState",
    "defaultSubtotal",
    "dragOff",
    "dragToCol",
    "dragToData",
    "dragToPage",
    "dragToRow",
    "hideNewItems",
    "hiddenLevel",
    "includeNewItemsInFilter",
    "insertBlankRow",
    "insertPageBreak",
    "maxSubtotal",
    "measureFilter",
    "minSubtotal",
    "multipleItemSelectionAllowed",
    "nonAutoSortDefault",
    "outline",
    "productSubtotal",
    "serverField",
    "showAll",
    "showDropDowns",
    "showPropAsCaption",
    "showPropCell",
    "showPropTip",
    "stdDevPSubtotal",
    "stdDevSubtotal",
    "subtotalTop",
    "sumSubtotal",
    "topAutoShow",
    "varPSubtotal",
    "varSubtotal",
];

const PIVOT_FIELD_UNSIGNED_ATTRIBUTES: &[&str] =
    &["itemPageCount", "numFmtId", "rankBy", "autoShowRankBy"];
const PIVOT_FIELD_STRING_ATTRIBUTES: &[&str] = &["name", "subtotalCaption", "uniqueMemberProperty"];

const ITEM_BOOLEAN_ATTRIBUTES: &[&str] = &["c", "d", "e", "f", "h", "m", "s"];
const ITEM_SIGNED_ATTRIBUTES: &[&str] = &["x"];
const ITEM_STRING_ATTRIBUTES: &[&str] = &["n", "t"];

const PAGE_FIELD_SIGNED_ATTRIBUTES: &[&str] = &["fld", "item", "hier"];
const PAGE_FIELD_STRING_ATTRIBUTES: &[&str] = &["name", "cap"];

const DATA_FIELD_SIGNED_ATTRIBUTES: &[&str] = &["fld", "baseField", "baseItem"];
const DATA_FIELD_UNSIGNED_ATTRIBUTES: &[&str] = &["numFmtId"];
const DATA_FIELD_STRING_ATTRIBUTES: &[&str] = &["name"];

const FILTER_SIGNED_ATTRIBUTES: &[&str] = &["fld", "evalOrder", "iMeasureFld", "iMeasureHier"];
const FILTER_UNSIGNED_ATTRIBUTES: &[&str] = &["id"];
const FILTER_STRING_ATTRIBUTES: &[&str] = &["name", "description", "stringValue1", "stringValue2"];

const STYLE_BOOLEAN_ATTRIBUTES: &[&str] = &[
    "showColHeaders",
    "showColStripes",
    "showLastColumn",
    "showRowHeaders",
    "showRowStripes",
];
const STYLE_STRING_ATTRIBUTES: &[&str] = &["name"];

const SUBTOTAL_ATTRIBUTES: &[&str] = &[
    "defaultSubtotal",
    "sumSubtotal",
    "countASubtotal",
    "avgSubtotal",
    "maxSubtotal",
    "minSubtotal",
    "productSubtotal",
    "countSubtotal",
    "stdDevSubtotal",
    "stdDevPSubtotal",
    "varSubtotal",
    "varPSubtotal",
];

const ROOT_CHILD_ORDER: &[&str] = &[
    "location",
    "pivotFields",
    "rowFields",
    "rowItems",
    "colFields",
    "colItems",
    "pageFields",
    "dataFields",
    "formats",
    "conditionalFormats",
    "chartFormats",
    "pivotHierarchies",
    "pivotTableStyleInfo",
    "filters",
    "rowHierarchiesUsage",
    "colHierarchiesUsage",
    "extLst",
];

#[derive(Clone, Debug)]
struct Relationship {
    id: String,
    kind: String,
    resolved_part: Option<String>,
}

/// A relationship-resolved PivotTable part.  File names and relationship IDs are deliberately
/// not assumed to follow Excel's usual numbering convention.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResolvedPivotTablePart {
    pub part: String,
    pub sheet: Option<String>,
    pub sheet_part: Option<String>,
    pub relationship_id: Option<String>,
    pub cache_part: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PivotTableWorkbookModel {
    pub workbook_part: String,
    pub tables: Vec<PivotTableModel>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PivotTableModel {
    pub part: String,
    pub sheet: Option<String>,
    pub sheet_part: Option<String>,
    pub relationship_id: Option<String>,
    pub cache_id: Option<u64>,
    pub cache_part: Option<String>,
    pub name: Option<String>,
    pub display: BTreeMap<String, Value>,
    pub location: Option<BTreeMap<String, Value>>,
    pub style: Option<BTreeMap<String, Value>>,
    pub fields: Vec<PivotFieldModel>,
    pub axes: AxisLayoutModel,
    pub filters: Vec<PivotFilterModel>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PivotFieldModel {
    pub index: usize,
    pub name: Option<String>,
    pub attributes: BTreeMap<String, Value>,
    pub subtotals: BTreeMap<String, Value>,
    pub sort: BTreeMap<String, Value>,
    pub items: Vec<PivotItemModel>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PivotItemModel {
    pub source_index: usize,
    pub cache_index: Option<usize>,
    pub label: String,
    pub value: Value,
    pub attributes: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AxisLayoutModel {
    pub rows: Vec<i64>,
    pub columns: Vec<i64>,
    pub pages: Vec<BTreeMap<String, Value>>,
    pub data: Vec<BTreeMap<String, Value>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PivotFilterModel {
    pub source_index: usize,
    pub attributes: BTreeMap<String, Value>,
}

#[derive(Clone, Debug)]
struct AttributeSpan {
    name: String,
    value: String,
    value_range: Range<usize>,
    full_range: Range<usize>,
}

#[derive(Clone, Copy, Debug)]
enum AttributeKind {
    Boolean,
    Signed,
    Unsigned,
    String,
    Enum(&'static [&'static str]),
}

#[derive(Clone, Debug)]
struct AttributeRule {
    name: &'static str,
    kind: AttributeKind,
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
    let relationship_path = if owner.is_empty() {
        "_rels/.rels".to_string()
    } else {
        relationship_part(owner)
    };
    let Some(bytes) = parts.get(&relationship_path) else {
        return Ok(HashMap::new());
    };
    let xml = std::str::from_utf8(bytes)
        .map_err(|error| format!("{relationship_path} UTF-8: {error}"))?;
    let document =
        Document::parse(xml).map_err(|error| format!("{relationship_path} XML: {error}"))?;
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

fn worksheet_parts(
    parts: &BTreeMap<String, Vec<u8>>,
    workbook_part: &str,
    workbook: Node<'_, '_>,
    relationships: &HashMap<String, Relationship>,
) -> Vec<(String, String)> {
    let mut worksheets = Vec::new();
    for sheet in workbook
        .descendants()
        .filter(|node| node.is_element() && local_name(*node) == "sheet")
    {
        let Some(id) = relationship_id(sheet) else {
            continue;
        };
        let Some(relationship) = relationships.get(&id) else {
            continue;
        };
        if !relationship.kind.ends_with("/worksheet") {
            continue;
        }
        let Some(part) = relationship.resolved_part.as_ref() else {
            continue;
        };
        if parts.contains_key(part) {
            worksheets.push((
                sheet.attribute("name").unwrap_or("").to_string(),
                part.clone(),
            ));
        }
    }
    if worksheets.is_empty() && workbook_part == "xl/workbook.xml" {
        let mut fallback: Vec<_> = parts
            .keys()
            .filter(|part| part.starts_with("xl/worksheets/") && part.ends_with(".xml"))
            .cloned()
            .collect();
        fallback.sort();
        worksheets.extend(
            fallback
                .into_iter()
                .enumerate()
                .map(|(index, part)| (format!("Sheet{}", index + 1), part)),
        );
    }
    worksheets
}

/// Resolve every live PivotTable via workbook -> worksheet -> PivotTable relationships, then
/// retain valid detached PivotTable definitions for diagnostics.  Detached discovery checks the
/// XML root instead of relying on a conventional `xl/pivotTables/pivotTableN.xml` name.
pub(crate) fn resolve_pivot_table_parts(
    parts: &BTreeMap<String, Vec<u8>>,
) -> Result<(String, Vec<ResolvedPivotTablePart>), String> {
    let workbook_part = office_document_part(parts)?;
    let (_, workbook_document) = parse_xml_part(parts, &workbook_part)?;
    let workbook_relationships = parse_relationships(parts, &workbook_part)?;
    let mut result = Vec::new();
    let mut referenced = HashSet::new();
    for (sheet_name, sheet_part) in worksheet_parts(
        parts,
        &workbook_part,
        workbook_document.root_element(),
        &workbook_relationships,
    ) {
        let (_, worksheet_document) = parse_xml_part(parts, &sheet_part)?;
        let worksheet_relationships = parse_relationships(parts, &sheet_part)?;
        for pivot_part_node in worksheet_document
            .descendants()
            .filter(|node| node.is_element() && local_name(*node) == "pivotTablePart")
        {
            let Some(id) = relationship_id(pivot_part_node) else {
                continue;
            };
            let Some(relationship) = worksheet_relationships.get(&id) else {
                continue;
            };
            if !relationship.kind.ends_with("/pivotTable") {
                continue;
            }
            let Some(part) = relationship.resolved_part.as_ref() else {
                continue;
            };
            let (_, table_document) = parse_xml_part(parts, part)?;
            if local_name(table_document.root_element()) != "pivotTableDefinition" {
                return Err(format!("{part} is not a pivotTableDefinition"));
            }
            let cache_part = parse_relationships(parts, part)?
                .values()
                .find(|relationship| relationship.kind.ends_with("/pivotCacheDefinition"))
                .and_then(|relationship| relationship.resolved_part.clone());
            result.push(ResolvedPivotTablePart {
                part: part.clone(),
                sheet: Some(sheet_name.clone()),
                sheet_part: Some(sheet_part.clone()),
                relationship_id: Some(relationship.id.clone()),
                cache_part,
            });
            referenced.insert(part.clone());
        }
    }

    for (part, bytes) in parts {
        if referenced.contains(part) || !part.ends_with(".xml") {
            continue;
        }
        let Ok(xml) = std::str::from_utf8(bytes) else {
            continue;
        };
        let Ok(document) = Document::parse(xml) else {
            continue;
        };
        if local_name(document.root_element()) != "pivotTableDefinition" {
            continue;
        }
        let cache_part = parse_relationships(parts, part)?
            .values()
            .find(|relationship| relationship.kind.ends_with("/pivotCacheDefinition"))
            .and_then(|relationship| relationship.resolved_part.clone());
        result.push(ResolvedPivotTablePart {
            part: part.clone(),
            sheet: None,
            sheet_part: None,
            relationship_id: None,
            cache_part,
        });
    }
    result.sort_by(|left, right| {
        left.sheet_part
            .cmp(&right.sheet_part)
            .then_with(|| left.part.cmp(&right.part))
    });
    Ok((workbook_part, result))
}

fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "1" | "true" | "on" => Some(true),
        "0" | "false" | "off" => Some(false),
        _ => None,
    }
}

fn attribute_json(node: Node<'_, '_>, name: &str, kind: AttributeKind) -> Option<Value> {
    let value = node.attribute(name)?;
    match kind {
        AttributeKind::Boolean => parse_bool(value).map(Value::Bool),
        AttributeKind::Signed => value.parse::<i64>().ok().map(Value::from),
        AttributeKind::Unsigned => value.parse::<u64>().ok().map(Value::from),
        AttributeKind::String | AttributeKind::Enum(_) => Some(Value::String(value.to_string())),
    }
}

fn collect_attributes(node: Node<'_, '_>, rules: &[AttributeRule]) -> BTreeMap<String, Value> {
    rules
        .iter()
        .filter_map(|rule| {
            attribute_json(node, rule.name, rule.kind).map(|value| (rule.name.to_string(), value))
        })
        .collect()
}

fn rules(
    booleans: &'static [&'static str],
    signed: &'static [&'static str],
    unsigned: &'static [&'static str],
    strings: &'static [&'static str],
) -> Vec<AttributeRule> {
    booleans
        .iter()
        .map(|name| AttributeRule {
            name,
            kind: AttributeKind::Boolean,
        })
        .chain(signed.iter().map(|name| AttributeRule {
            name,
            kind: AttributeKind::Signed,
        }))
        .chain(unsigned.iter().map(|name| AttributeRule {
            name,
            kind: AttributeKind::Unsigned,
        }))
        .chain(strings.iter().map(|name| AttributeRule {
            name,
            kind: AttributeKind::String,
        }))
        .collect()
}

fn root_rules() -> Vec<AttributeRule> {
    rules(
        ROOT_BOOLEAN_ATTRIBUTES,
        &[],
        ROOT_UNSIGNED_ATTRIBUTES,
        ROOT_STRING_ATTRIBUTES,
    )
}

fn pivot_field_rules() -> Vec<AttributeRule> {
    let mut result = rules(
        PIVOT_FIELD_BOOLEAN_ATTRIBUTES,
        &[],
        PIVOT_FIELD_UNSIGNED_ATTRIBUTES,
        PIVOT_FIELD_STRING_ATTRIBUTES,
    );
    result.push(AttributeRule {
        name: "axis",
        kind: AttributeKind::Enum(&["axisRow", "axisCol", "axisPage", "axisValues"]),
    });
    result.push(AttributeRule {
        name: "sortType",
        kind: AttributeKind::Enum(&["manual", "ascending", "descending"]),
    });
    result
}

fn data_field_rules() -> Vec<AttributeRule> {
    let mut result = rules(
        &[],
        DATA_FIELD_SIGNED_ATTRIBUTES,
        DATA_FIELD_UNSIGNED_ATTRIBUTES,
        DATA_FIELD_STRING_ATTRIBUTES,
    );
    result.push(AttributeRule {
        name: "subtotal",
        kind: AttributeKind::Enum(&[
            "average",
            "count",
            "countNums",
            "max",
            "min",
            "product",
            "stdDev",
            "stdDevp",
            "sum",
            "var",
            "varp",
        ]),
    });
    result.push(AttributeRule {
        name: "showDataAs",
        kind: AttributeKind::Enum(&[
            "normal",
            "difference",
            "percent",
            "percentDiff",
            "runTotal",
            "percentOfRow",
            "percentOfCol",
            "percentOfTotal",
            "index",
        ]),
    });
    result
}

fn filter_rules() -> Vec<AttributeRule> {
    let mut result = rules(
        &[],
        FILTER_SIGNED_ATTRIBUTES,
        FILTER_UNSIGNED_ATTRIBUTES,
        FILTER_STRING_ATTRIBUTES,
    );
    result.push(AttributeRule {
        name: "type",
        kind: AttributeKind::Enum(&[
            "unknown",
            "count",
            "percent",
            "sum",
            "captionEqual",
            "captionNotEqual",
            "captionBeginsWith",
            "captionNotBeginsWith",
            "captionEndsWith",
            "captionNotEndsWith",
            "captionContains",
            "captionNotContains",
            "captionGreaterThan",
            "captionGreaterThanOrEqual",
            "captionLessThan",
            "captionLessThanOrEqual",
            "captionBetween",
            "captionNotBetween",
            "valueEqual",
            "valueNotEqual",
            "valueGreaterThan",
            "valueGreaterThanOrEqual",
            "valueLessThan",
            "valueLessThanOrEqual",
            "valueBetween",
            "valueNotBetween",
            "dateEqual",
            "dateNotEqual",
            "dateOlderThan",
            "dateOlderThanOrEqual",
            "dateNewerThan",
            "dateNewerThanOrEqual",
            "dateBetween",
            "dateNotBetween",
            "tomorrow",
            "today",
            "yesterday",
            "nextWeek",
            "thisWeek",
            "lastWeek",
            "nextMonth",
            "thisMonth",
            "lastMonth",
            "nextQuarter",
            "thisQuarter",
            "lastQuarter",
            "nextYear",
            "thisYear",
            "lastYear",
            "yearToDate",
            "Q1",
            "Q2",
            "Q3",
            "Q4",
            "M1",
            "M2",
            "M3",
            "M4",
            "M5",
            "M6",
            "M7",
            "M8",
            "M9",
            "M10",
            "M11",
            "M12",
        ]),
    });
    result
}

#[derive(Clone, Debug)]
struct SharedItemModel {
    label: String,
    value: Value,
}

#[derive(Clone, Debug)]
struct CacheFieldModel {
    name: Option<String>,
    shared_items: Vec<SharedItemModel>,
}

fn shared_item_model(node: Node<'_, '_>) -> SharedItemModel {
    let raw = node.attribute("v").unwrap_or("");
    match local_name(node) {
        "s" => SharedItemModel {
            label: raw.to_string(),
            value: Value::String(raw.to_string()),
        },
        "n" => SharedItemModel {
            label: raw.to_string(),
            value: raw
                .parse::<f64>()
                .ok()
                .and_then(Number::from_f64)
                .map(Value::Number)
                .unwrap_or_else(|| Value::String(raw.to_string())),
        },
        "b" => {
            let value = parse_bool(raw).unwrap_or(false);
            SharedItemModel {
                label: if value { "TRUE" } else { "FALSE" }.to_string(),
                value: Value::Bool(value),
            }
        }
        "m" => SharedItemModel {
            label: "(blank)".to_string(),
            value: Value::Null,
        },
        // Dates and errors remain strings so the UI does not lose their lexical precision or
        // locale-neutral OOXML representation.
        "d" | "e" | "x" => SharedItemModel {
            label: raw.to_string(),
            value: Value::String(raw.to_string()),
        },
        other => SharedItemModel {
            label: if raw.is_empty() {
                other.to_string()
            } else {
                raw.to_string()
            },
            value: if raw.is_empty() {
                Value::Null
            } else {
                Value::String(raw.to_string())
            },
        },
    }
}

fn cache_fields(cache_xml: Option<&str>) -> Vec<CacheFieldModel> {
    let Some(cache_xml) = cache_xml else {
        return Vec::new();
    };
    let Ok(document) = Document::parse(cache_xml) else {
        return Vec::new();
    };
    let root = document.root_element();
    direct_child(root, "cacheFields")
        .into_iter()
        .flat_map(|container| direct_children(container, "cacheField"))
        .map(|field| CacheFieldModel {
            name: field.attribute("name").map(str::to_string),
            shared_items: direct_child(field, "sharedItems")
                .into_iter()
                .flat_map(|container| container.children().filter(|node| node.is_element()))
                .map(shared_item_model)
                .collect(),
        })
        .collect()
}

fn parse_axis_fields(root: Node<'_, '_>, container_name: &str) -> Vec<i64> {
    direct_child(root, container_name)
        .into_iter()
        .flat_map(|container| direct_children(container, "field"))
        .filter_map(|field| field.attribute("x")?.parse::<i64>().ok())
        .collect()
}

fn parse_repeated_attribute_nodes(
    root: Node<'_, '_>,
    container_name: &str,
    child_name: &str,
    node_rules: &[AttributeRule],
) -> Vec<BTreeMap<String, Value>> {
    direct_child(root, container_name)
        .into_iter()
        .flat_map(|container| direct_children(container, child_name))
        .enumerate()
        .map(|(source_index, node)| {
            let mut attributes = collect_attributes(node, node_rules);
            attributes.insert("sourceIndex".to_string(), Value::from(source_index));
            attributes
        })
        .collect()
}

fn parse_pivot_table_definition(
    resolved: &ResolvedPivotTablePart,
    table_xml: &str,
    cache_xml: Option<&str>,
) -> Result<PivotTableModel, String> {
    let document =
        Document::parse(table_xml).map_err(|error| format!("{} XML: {error}", resolved.part))?;
    let root = document.root_element();
    if local_name(root) != "pivotTableDefinition" {
        return Err(format!("{} is not a pivotTableDefinition", resolved.part));
    }
    let cache_fields = cache_fields(cache_xml);
    let pivot_fields = direct_child(root, "pivotFields");
    let fields = pivot_fields
        .into_iter()
        .flat_map(|container| direct_children(container, "pivotField"))
        .enumerate()
        .map(|(field_index, field)| {
            let attributes = collect_attributes(field, &pivot_field_rules());
            let subtotals = SUBTOTAL_ATTRIBUTES
                .iter()
                .filter_map(|name| {
                    attribute_json(field, name, AttributeKind::Boolean)
                        .map(|value| ((*name).to_string(), value))
                })
                .collect();
            let sort = [
                "sortType",
                "dataSourceSort",
                "rankBy",
                "autoShow",
                "topAutoShow",
            ]
            .into_iter()
            .filter_map(|name| {
                attributes
                    .get(name)
                    .cloned()
                    .map(|value| (name.to_string(), value))
            })
            .collect();
            let item_rules = rules(
                ITEM_BOOLEAN_ATTRIBUTES,
                ITEM_SIGNED_ATTRIBUTES,
                &[],
                ITEM_STRING_ATTRIBUTES,
            );
            let items = direct_child(field, "items")
                .into_iter()
                .flat_map(|container| direct_children(container, "item"))
                .enumerate()
                .map(|(source_index, item)| {
                    let cache_index = item
                        .attribute("x")
                        .and_then(|value| value.parse::<usize>().ok());
                    let shared = cache_index.and_then(|cache_index| {
                        cache_fields
                            .get(field_index)
                            .and_then(|field| field.shared_items.get(cache_index))
                    });
                    let label = item
                        .attribute("n")
                        .map(str::to_string)
                        .or_else(|| shared.map(|item| item.label.clone()))
                        .or_else(|| item.attribute("t").map(str::to_string))
                        .unwrap_or_else(|| cache_index.unwrap_or(source_index).to_string());
                    PivotItemModel {
                        source_index,
                        cache_index,
                        label,
                        value: shared.map(|item| item.value.clone()).unwrap_or(Value::Null),
                        attributes: collect_attributes(item, &item_rules),
                    }
                })
                .collect();
            PivotFieldModel {
                index: field_index,
                name: cache_fields
                    .get(field_index)
                    .and_then(|field| field.name.clone()),
                attributes,
                subtotals,
                sort,
                items,
            }
        })
        .collect();

    let location_rules = rules(&[], &[], LOCATION_UNSIGNED_ATTRIBUTES, &["ref"]);
    let style_rules = rules(STYLE_BOOLEAN_ATTRIBUTES, &[], &[], STYLE_STRING_ATTRIBUTES);
    let filters = direct_child(root, "filters")
        .into_iter()
        .flat_map(|container| direct_children(container, "filter"))
        .enumerate()
        .map(|(source_index, filter)| PivotFilterModel {
            source_index,
            attributes: collect_attributes(filter, &filter_rules()),
        })
        .collect();
    Ok(PivotTableModel {
        part: resolved.part.clone(),
        sheet: resolved.sheet.clone(),
        sheet_part: resolved.sheet_part.clone(),
        relationship_id: resolved.relationship_id.clone(),
        cache_id: root
            .attribute("cacheId")
            .and_then(|value| value.parse().ok()),
        cache_part: resolved.cache_part.clone(),
        name: root.attribute("name").map(str::to_string),
        display: collect_attributes(root, &root_rules()),
        location: direct_child(root, "location")
            .map(|location| collect_attributes(location, &location_rules)),
        style: direct_child(root, "pivotTableStyleInfo")
            .map(|style| collect_attributes(style, &style_rules)),
        fields,
        axes: AxisLayoutModel {
            rows: parse_axis_fields(root, "rowFields"),
            columns: parse_axis_fields(root, "colFields"),
            pages: parse_repeated_attribute_nodes(
                root,
                "pageFields",
                "pageField",
                &rules(
                    &[],
                    PAGE_FIELD_SIGNED_ATTRIBUTES,
                    &[],
                    PAGE_FIELD_STRING_ATTRIBUTES,
                ),
            ),
            data: parse_repeated_attribute_nodes(
                root,
                "dataFields",
                "dataField",
                &data_field_rules(),
            ),
        },
        filters,
    })
}

/// Parse every native PivotTable into a stable JSON model suitable for the API/UI layer.
pub(crate) fn parse_pivot_table_model(parts: &BTreeMap<String, Vec<u8>>) -> Result<Value, String> {
    let (workbook_part, resolved) = resolve_pivot_table_parts(parts)?;
    let mut tables = Vec::new();
    for table in resolved {
        let table_xml = std::str::from_utf8(
            parts
                .get(&table.part)
                .ok_or_else(|| format!("missing OPC part {}", table.part))?,
        )
        .map_err(|error| format!("{} UTF-8: {error}", table.part))?;
        let cache_xml = table
            .cache_part
            .as_ref()
            .and_then(|part| parts.get(part))
            .and_then(|bytes| std::str::from_utf8(bytes).ok());
        tables.push(parse_pivot_table_definition(&table, table_xml, cache_xml)?);
    }
    serde_json::to_value(PivotTableWorkbookModel {
        workbook_part,
        tables,
    })
    .map_err(|error| format!("PivotTable model JSON: {error}"))
}

fn root_open_tag_range_for(xml: &str, expected: &str) -> Result<Range<usize>, String> {
    let document = Document::parse(xml).map_err(|error| format!("PivotTable XML: {error}"))?;
    let root = document.root_element();
    if local_name(root) != expected {
        return Err(format!("root is not {expected}"));
    }
    open_tag_range(xml, root.range().start)
}

fn lexical_root_open_tag_range(fragment: &str, expected: &str) -> Result<Range<usize>, String> {
    let bytes = fragment.as_bytes();
    let mut cursor = 0usize;
    loop {
        let relative = fragment[cursor..]
            .find('<')
            .ok_or_else(|| format!("XML fragment has no {expected} element"))?;
        let start = cursor + relative;
        let marker = *bytes
            .get(start + 1)
            .ok_or_else(|| "truncated XML element".to_string())?;
        if matches!(marker, b'?' | b'!') {
            let end = if marker == b'?' {
                fragment[start + 2..]
                    .find("?>")
                    .map(|offset| start + 2 + offset + 2)
            } else if fragment[start..].starts_with("<!--") {
                fragment[start + 4..]
                    .find("-->")
                    .map(|offset| start + 4 + offset + 3)
            } else {
                fragment[start + 2..]
                    .find('>')
                    .map(|offset| start + 2 + offset + 1)
            }
            .ok_or_else(|| "unterminated XML declaration/comment".to_string())?;
            cursor = end;
            continue;
        }
        if marker == b'/' {
            return Err(format!("XML fragment has no opening {expected} element"));
        }
        let name_start = start + 1;
        let mut name_end = name_start;
        while name_end < bytes.len()
            && !bytes[name_end].is_ascii_whitespace()
            && !matches!(bytes[name_end], b'/' | b'>')
        {
            name_end += 1;
        }
        let qualified_name = &fragment[name_start..name_end];
        let local = qualified_name
            .rsplit_once(':')
            .map(|(_, local)| local)
            .unwrap_or(qualified_name);
        if local != expected {
            return Err(format!("root is not {expected}"));
        }
        return open_tag_range(fragment, start);
    }
}

fn open_tag_range(xml: &str, start: usize) -> Result<Range<usize>, String> {
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
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn patch_start_tag_attribute(
    xml: &str,
    tag_range: Range<usize>,
    name: &str,
    requested: Option<&str>,
    semantically_equal: impl Fn(&str) -> bool,
) -> Result<String, String> {
    let tag = &xml[tag_range.clone()];
    let existing = scan_start_tag_attributes(tag)?
        .into_iter()
        .find(|attribute| attribute.name == name);
    match (existing, requested) {
        (None, None) => Ok(xml.to_string()),
        (Some(attribute), None) => {
            let mut result = xml.to_string();
            result.replace_range(
                tag_range.start + attribute.full_range.start
                    ..tag_range.start + attribute.full_range.end,
                "",
            );
            Ok(result)
        }
        (Some(attribute), Some(_)) if semantically_equal(attribute.value.as_str()) => {
            Ok(xml.to_string())
        }
        (Some(attribute), Some(value)) => {
            let mut result = xml.to_string();
            result.replace_range(
                tag_range.start + attribute.value_range.start
                    ..tag_range.start + attribute.value_range.end,
                value,
            );
            Ok(result)
        }
        (None, Some(value)) => {
            let insert = if tag.as_bytes().get(tag.len().saturating_sub(2)) == Some(&b'/') {
                tag_range.end - 2
            } else {
                tag_range.end - 1
            };
            let mut result = xml.to_string();
            result.insert_str(insert, &format!(" {name}=\"{value}\""));
            Ok(result)
        }
    }
}

fn rule_map(rules: &[AttributeRule]) -> HashMap<&str, AttributeKind> {
    rules.iter().map(|rule| (rule.name, rule.kind)).collect()
}

fn patch_element_attributes(
    fragment: &str,
    expected: &str,
    patch: &Map<String, Value>,
    rules: &[AttributeRule],
) -> Result<String, String> {
    // Existing child fragments may use prefixes declared only on the PivotTable root.  Lexical
    // start-tag inspection avoids rejecting those valid inherited namespaces.  The whole part is
    // parsed at the public entry point and caller-supplied rawXml is validated separately.
    let _ = lexical_root_open_tag_range(fragment, expected)?;
    let allowed = rule_map(rules);
    for key in patch.keys() {
        if !allowed.contains_key(key.as_str()) {
            return Err(format!("unsupported {expected} attribute {key}"));
        }
    }
    let mut result = fragment.to_string();
    for (name, value) in patch {
        let kind = allowed[name.as_str()];
        let tag_range = lexical_root_open_tag_range(&result, expected)?;
        if value.is_null() {
            result = patch_start_tag_attribute(&result, tag_range, name, None, |_| false)?;
            continue;
        }
        let (encoded, equal): (String, Box<dyn Fn(&str) -> bool>) = match kind {
            AttributeKind::Boolean => {
                let requested = value
                    .as_bool()
                    .ok_or_else(|| format!("{expected}.{name} must be boolean or null"))?;
                (
                    if requested { "1" } else { "0" }.to_string(),
                    Box::new(move |existing| parse_bool(existing) == Some(requested)),
                )
            }
            AttributeKind::Signed => {
                let requested = value
                    .as_i64()
                    .ok_or_else(|| format!("{expected}.{name} must be a signed integer or null"))?;
                (
                    requested.to_string(),
                    Box::new(move |existing| existing.parse::<i64>().ok() == Some(requested)),
                )
            }
            AttributeKind::Unsigned => {
                let requested = value.as_u64().ok_or_else(|| {
                    format!("{expected}.{name} must be an unsigned integer or null")
                })?;
                (
                    requested.to_string(),
                    Box::new(move |existing| existing.parse::<u64>().ok() == Some(requested)),
                )
            }
            AttributeKind::String => {
                let requested = value
                    .as_str()
                    .ok_or_else(|| format!("{expected}.{name} must be a string or null"))?;
                let encoded = xml_escape_attribute(requested);
                let compare = encoded.clone();
                (encoded, Box::new(move |existing| existing == compare))
            }
            AttributeKind::Enum(values) => {
                let requested = value
                    .as_str()
                    .ok_or_else(|| format!("{expected}.{name} must be a string or null"))?;
                if !values.contains(&requested) {
                    return Err(format!(
                        "unsupported {expected}.{name} value {requested}; expected one of {}",
                        values.join(", ")
                    ));
                }
                let requested = requested.to_string();
                let compare = requested.clone();
                (requested, Box::new(move |existing| existing == compare))
            }
        };
        result = patch_start_tag_attribute(&result, tag_range, name, Some(&encoded), equal)?;
    }
    Ok(result)
}

fn apply_ranges(
    xml: &str,
    mut replacements: Vec<(Range<usize>, String)>,
) -> Result<String, String> {
    replacements.sort_by(|left, right| right.0.start.cmp(&left.0.start));
    let mut previous_start = xml.len();
    let mut result = xml.to_string();
    for (range, replacement) in replacements {
        if range.start > range.end || range.end > previous_start || range.end > result.len() {
            return Err("overlapping or invalid PivotTable XML patches".to_string());
        }
        previous_start = range.start;
        result.replace_range(range, &replacement);
    }
    Ok(result)
}

fn object_field<'a>(
    object: &'a Map<String, Value>,
    name: &str,
) -> Result<Option<&'a Map<String, Value>>, String> {
    object
        .get(name)
        .map(|value| {
            value
                .as_object()
                .ok_or_else(|| format!("PivotTable patch.{name} must be an object"))
        })
        .transpose()
}

fn patch_direct_child(
    xml: &str,
    child_name: &str,
    patch: &Map<String, Value>,
    rules: &[AttributeRule],
) -> Result<String, String> {
    let document = Document::parse(xml).map_err(|error| format!("PivotTable XML: {error}"))?;
    let root = document.root_element();
    let child = direct_child(root, child_name)
        .ok_or_else(|| format!("pivotTableDefinition has no {child_name}"))?;
    let range = child.range();
    let edited = patch_element_attributes(&xml[range.clone()], child_name, patch, rules)?;
    if edited == xml[range.clone()] {
        return Ok(xml.to_string());
    }
    apply_ranges(xml, vec![(range, edited)])
}

fn nested_object<'a>(
    object: &'a Map<String, Value>,
    name: &str,
) -> Result<Option<&'a Map<String, Value>>, String> {
    object
        .get(name)
        .map(|value| {
            value
                .as_object()
                .ok_or_else(|| format!("{name} must be an object"))
        })
        .transpose()
}

fn merge_attribute_sections(
    object: &Map<String, Value>,
    section_names: &[&str],
    reserved: &[&str],
) -> Result<Map<String, Value>, String> {
    let mut result = Map::new();
    for section_name in section_names {
        if let Some(section) = nested_object(object, section_name)? {
            result.extend(section.clone());
        }
    }
    for (key, value) in object {
        if section_names.contains(&key.as_str()) || reserved.contains(&key.as_str()) {
            continue;
        }
        result.insert(key.clone(), value.clone());
    }
    Ok(result)
}

fn wrap_inherited_namespace_fragment(fragment: &str) -> (String, usize) {
    // A sliced child may use a producer-specific prefix declared only on the PivotTable root.
    // Declare every QName-looking prefix on a temporary wrapper solely to discover byte ranges.
    let bytes = fragment.as_bytes();
    let mut prefixes = BTreeSet::new();
    for (colon, byte) in bytes.iter().enumerate() {
        if *byte != b':' || colon == 0 {
            continue;
        }
        let mut start = colon;
        while start > 0
            && (bytes[start - 1].is_ascii_alphanumeric()
                || matches!(bytes[start - 1], b'_' | b'-' | b'.'))
        {
            start -= 1;
        }
        let prefix = &fragment[start..colon];
        if prefix.is_empty()
            || prefix == "xml"
            || prefix == "xmlns"
            || !prefix
                .as_bytes()
                .first()
                .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
        {
            continue;
        }
        prefixes.insert(prefix.to_string());
    }
    let mut opening = "<unicell-fragment-root".to_string();
    for (index, prefix) in prefixes.iter().enumerate() {
        opening.push_str(&format!(
            " xmlns:{prefix}=\"urn:unicell:inherited:{index}\""
        ));
    }
    opening.push('>');
    let offset = opening.len();
    (
        format!("{opening}{fragment}</unicell-fragment-root>"),
        offset,
    )
}

fn patch_pivot_field_items(
    field_fragment: &str,
    patch: &Map<String, Value>,
) -> Result<String, String> {
    let hidden_items: Option<BTreeSet<usize>> = patch
        .get("hiddenItems")
        .map(|value| {
            value
                .as_array()
                .ok_or_else(|| "PivotField hiddenItems must be an array".to_string())?
                .iter()
                .map(|value| {
                    value
                        .as_u64()
                        .and_then(|value| usize::try_from(value).ok())
                        .ok_or_else(|| "PivotField hiddenItems entries must be indices".to_string())
                })
                .collect::<Result<BTreeSet<_>, _>>()
        })
        .transpose()?;
    let item_patches = patch
        .get("items")
        .map(|value| {
            value
                .as_array()
                .ok_or_else(|| "PivotField items must be an array".to_string())
        })
        .transpose()?;
    if hidden_items.is_none() && item_patches.is_none() {
        return Ok(field_fragment.to_string());
    }
    let (wrapped, fragment_offset) = wrap_inherited_namespace_fragment(field_fragment);
    let document = Document::parse(&wrapped).map_err(|error| format!("pivotField XML: {error}"))?;
    let root = direct_child(document.root_element(), "pivotField")
        .ok_or_else(|| "wrapped XML has no pivotField".to_string())?;
    let Some(items) = direct_child(root, "items") else {
        if hidden_items.as_ref().is_some_and(|items| !items.is_empty())
            || item_patches.is_some_and(|items| !items.is_empty())
        {
            return Err("pivotField has no items collection".to_string());
        }
        return Ok(field_fragment.to_string());
    };
    let item_nodes: Vec<_> = direct_children(items, "item").collect();
    let mut patches_by_index: HashMap<usize, Map<String, Value>> = HashMap::new();
    if let Some(hidden) = hidden_items {
        for index in 0..item_nodes.len() {
            let mut attributes = Map::new();
            attributes.insert(
                "h".to_string(),
                if hidden.contains(&index) {
                    Value::Bool(true)
                } else {
                    Value::Null
                },
            );
            patches_by_index.insert(index, attributes);
        }
        if hidden.iter().any(|index| *index >= item_nodes.len()) {
            return Err("PivotField hiddenItems contains an out-of-range item index".to_string());
        }
    }
    if let Some(item_patches) = item_patches {
        for value in item_patches {
            let object = value
                .as_object()
                .ok_or_else(|| "PivotField item patch must be an object".to_string())?;
            let index = object
                .get("sourceIndex")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| "PivotField item patch requires sourceIndex".to_string())?;
            if index >= item_nodes.len() {
                return Err(format!(
                    "PivotField item sourceIndex {index} is out of range"
                ));
            }
            let attributes = object
                .get("attributes")
                .map(|value| {
                    value
                        .as_object()
                        .cloned()
                        .ok_or_else(|| "PivotField item attributes must be an object".to_string())
                })
                .transpose()?
                .unwrap_or_else(|| {
                    object
                        .iter()
                        .filter(|(key, _)| key.as_str() != "sourceIndex")
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect()
                });
            patches_by_index
                .entry(index)
                .or_default()
                .extend(attributes);
        }
    }
    let item_rules = rules(
        ITEM_BOOLEAN_ATTRIBUTES,
        ITEM_SIGNED_ATTRIBUTES,
        &[],
        ITEM_STRING_ATTRIBUTES,
    );
    let mut replacements = Vec::new();
    for (index, attributes) in patches_by_index {
        let node = item_nodes[index];
        let absolute_range = node.range();
        let range = absolute_range.start - fragment_offset..absolute_range.end - fragment_offset;
        let edited = patch_element_attributes(
            &field_fragment[range.clone()],
            "item",
            &attributes,
            &item_rules,
        )?;
        if edited != field_fragment[range.clone()] {
            replacements.push((range, edited));
        }
    }
    apply_ranges(field_fragment, replacements)
}

fn patch_pivot_fields(xml: &str, patches: &[Value]) -> Result<String, String> {
    if patches.is_empty() {
        return Ok(xml.to_string());
    }
    let document = Document::parse(xml).map_err(|error| format!("PivotTable XML: {error}"))?;
    let root = document.root_element();
    let container = direct_child(root, "pivotFields")
        .ok_or_else(|| "pivotTableDefinition has no pivotFields".to_string())?;
    let fields: Vec<_> = direct_children(container, "pivotField").collect();
    let mut replacements = Vec::new();
    let mut seen = HashSet::new();
    for value in patches {
        let object = value
            .as_object()
            .ok_or_else(|| "PivotTable fields entries must be objects".to_string())?;
        let index = object
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| "PivotField patch requires index".to_string())?;
        if index >= fields.len() {
            return Err(format!("PivotField index {index} is out of range"));
        }
        if !seen.insert(index) {
            return Err(format!(
                "PivotField index {index} is patched more than once"
            ));
        }
        let attributes = merge_attribute_sections(
            object,
            &["attributes", "subtotals", "sort"],
            &["index", "hiddenItems", "items"],
        )?;
        let range = fields[index].range();
        let mut edited = patch_element_attributes(
            &xml[range.clone()],
            "pivotField",
            &attributes,
            &pivot_field_rules(),
        )?;
        edited = patch_pivot_field_items(&edited, object)?;
        if edited != xml[range.clone()] {
            replacements.push((range, edited));
        }
    }
    apply_ranges(xml, replacements)
}

fn signed_field_spec(value: &Value) -> Result<Map<String, Value>, String> {
    let field = value
        .as_i64()
        .ok_or_else(|| "axis field entries must be signed integers".to_string())?;
    Ok(Map::from_iter([("x".to_string(), Value::from(field))]))
}

fn field_node_rules() -> Vec<AttributeRule> {
    rules(&[], &["x"], &[], &[])
}

fn normalize_repeated_spec(
    value: &Value,
    reserved: &[&str],
) -> Result<(Option<usize>, Option<String>, Map<String, Value>), String> {
    let object = value
        .as_object()
        .ok_or_else(|| "repeated PivotTable child specification must be an object".to_string())?;
    let source_index = object
        .get("sourceIndex")
        .map(|value| {
            value
                .as_u64()
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| "sourceIndex must be a non-negative integer".to_string())
        })
        .transpose()?;
    let raw_xml = object
        .get("rawXml")
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| "rawXml must be a string".to_string())
        })
        .transpose()?;
    let attributes = object
        .get("attributes")
        .map(|value| {
            value
                .as_object()
                .cloned()
                .ok_or_else(|| "attributes must be an object".to_string())
        })
        .transpose()?
        .unwrap_or_else(|| {
            object
                .iter()
                .filter(|(key, _)| !reserved.contains(&key.as_str()))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        });
    Ok((source_index, raw_xml, attributes))
}

fn element_prefix(fragment: &str) -> &str {
    fragment
        .strip_prefix('<')
        .and_then(|rest| {
            rest.split_once(|character: char| {
                character.is_whitespace() || character == '>' || character == '/'
            })
        })
        .map(|(name, _)| name)
        .and_then(|name| name.rsplit_once(':').map(|(prefix, _)| prefix))
        .unwrap_or("")
}

fn new_empty_element(name: &str, prefix: &str) -> String {
    if prefix.is_empty() {
        format!("<{name}/>")
    } else {
        format!("<{prefix}:{name}/>")
    }
}

fn patch_container_count(fragment: &str, count: usize, expected: &str) -> Result<String, String> {
    patch_element_attributes(
        fragment,
        expected,
        &Map::from_iter([("count".to_string(), Value::from(count))]),
        &[AttributeRule {
            name: "count",
            kind: AttributeKind::Unsigned,
        }],
    )
}

fn root_close_tag_start(xml: &str, root: Node<'_, '_>) -> Result<usize, String> {
    xml[root.range()]
        .rfind("</")
        .map(|offset| root.range().start + offset)
        .ok_or_else(|| "pivotTableDefinition has no closing tag".to_string())
}

fn insert_root_child(xml: &str, child_name: &str, fragment: &str) -> Result<String, String> {
    let document = Document::parse(xml).map_err(|error| format!("PivotTable XML: {error}"))?;
    let root = document.root_element();
    let requested_order = ROOT_CHILD_ORDER
        .iter()
        .position(|name| *name == child_name)
        .ok_or_else(|| format!("unknown PivotTable child {child_name}"))?;
    let insert = root
        .children()
        .filter(|child| child.is_element())
        .find(|child| {
            ROOT_CHILD_ORDER
                .iter()
                .position(|name| *name == local_name(*child))
                .is_some_and(|order| order > requested_order)
        })
        .map(|child| child.range().start)
        .unwrap_or(root_close_tag_start(xml, root)?);
    let mut result = xml.to_string();
    result.insert_str(insert, fragment);
    Ok(result)
}

fn rewrite_repeated_container(
    xml: &str,
    container_name: &str,
    child_name: &str,
    specs: &[(Option<usize>, Option<String>, Map<String, Value>)],
    node_rules: &[AttributeRule],
) -> Result<String, String> {
    let document = Document::parse(xml).map_err(|error| format!("PivotTable XML: {error}"))?;
    let root = document.root_element();
    let Some(container) = direct_child(root, container_name) else {
        if specs.is_empty() {
            return Ok(xml.to_string());
        }
        let prefix = element_prefix(&xml[root_open_tag_range_for(xml, "pivotTableDefinition")?]);
        let mut children = String::new();
        for (_, raw_xml, attributes) in specs {
            let raw = raw_xml
                .clone()
                .unwrap_or_else(|| new_empty_element(child_name, prefix));
            let document = Document::parse(&raw)
                .map_err(|error| format!("new {child_name} rawXml: {error}"))?;
            if local_name(document.root_element()) != child_name {
                return Err(format!("new rawXml root must be {child_name}"));
            }
            children.push_str(&patch_element_attributes(
                &raw, child_name, attributes, node_rules,
            )?);
        }
        let qualified_container = if prefix.is_empty() {
            container_name.to_string()
        } else {
            format!("{prefix}:{container_name}")
        };
        let fragment = format!(
            "<{qualified_container} count=\"{}\">{children}</{qualified_container}>",
            specs.len()
        );
        return insert_root_child(xml, container_name, &fragment);
    };
    let children: Vec<_> = direct_children(container, child_name).collect();
    if specs.is_empty() {
        let has_unknown_attributes = container
            .attributes()
            .any(|attribute| attribute.name() != "count");
        let has_unknown_children = container
            .children()
            .any(|child| child.is_element() && local_name(child) != child_name);
        if !has_unknown_attributes && !has_unknown_children {
            // OOXML repeated containers generally require at least one known child when present.
            // Excel therefore represents an empty axis/filter collection by omitting its
            // container.  Preserve a producer-specific container only when it carries opaque
            // attributes or children that cannot safely be discarded.
            return apply_ranges(xml, vec![(container.range(), String::new())]);
        }
    }
    let prefix = children
        .first()
        .map(|child| element_prefix(&xml[child.range()]))
        .unwrap_or_else(|| element_prefix(&xml[container.range()]));
    let mut used = HashSet::new();
    let mut fragments = Vec::new();
    for (position, (source_index, raw_xml, attributes)) in specs.iter().enumerate() {
        let selected = if let Some(index) = source_index {
            if *index >= children.len() {
                return Err(format!(
                    "{container_name} sourceIndex {index} is out of range"
                ));
            }
            if !used.insert(*index) {
                return Err(format!(
                    "{container_name} sourceIndex {index} is used more than once"
                ));
            }
            Some(*index)
        } else if position < children.len() && used.insert(position) {
            Some(position)
        } else {
            None
        };
        let raw = if let Some(raw_xml) = raw_xml {
            let parsed = Document::parse(raw_xml)
                .map_err(|error| format!("new {child_name} rawXml: {error}"))?;
            if local_name(parsed.root_element()) != child_name {
                return Err(format!("new rawXml root must be {child_name}"));
            }
            raw_xml.clone()
        } else if let Some(index) = selected {
            xml[children[index].range()].to_string()
        } else {
            new_empty_element(child_name, prefix)
        };
        fragments.push(patch_element_attributes(
            &raw, child_name, attributes, node_rules,
        )?);
    }

    // The common same-cardinality path replaces only existing child ranges.  This leaves every
    // comment, whitespace run, and unknown direct child exactly where it was.
    if specs.len() == children.len() {
        let replacements = children
            .iter()
            .zip(fragments)
            .filter_map(|(node, fragment)| {
                let range = node.range();
                (fragment != xml[range.clone()]).then_some((range, fragment))
            })
            .collect();
        let edited = apply_ranges(xml, replacements)?;
        let document = Document::parse(&edited)
            .map_err(|error| format!("PivotTable XML after {container_name}: {error}"))?;
        let container = direct_child(document.root_element(), container_name).unwrap();
        let range = container.range();
        let counted = patch_container_count(&edited[range.clone()], specs.len(), container_name)?;
        return apply_ranges(&edited, vec![(range, counted)]);
    }

    // For insertion/removal, strip only the known children, retain all other bytes in the
    // container, and insert the requested sequence at the first former child position.
    let container_range = container.range();
    let mut container_fragment = xml[container_range.clone()].to_string();
    let local_ranges: Vec<_> = children
        .iter()
        .map(|child| {
            let range = child.range();
            range.start - container_range.start..range.end - container_range.start
        })
        .collect();
    let insertion = local_ranges
        .first()
        .map(|range| range.start)
        .unwrap_or_else(|| {
            container_fragment
                .rfind("</")
                .unwrap_or(container_fragment.len())
        });
    let removals: Vec<_> = local_ranges
        .into_iter()
        .map(|range| (range, String::new()))
        .collect();
    container_fragment = apply_ranges(&container_fragment, removals)?;
    container_fragment.insert_str(insertion, &fragments.concat());
    container_fragment = patch_container_count(&container_fragment, specs.len(), container_name)?;
    apply_ranges(xml, vec![(container_range, container_fragment)])
}

fn parse_axis_specs(
    value: &Value,
) -> Result<Vec<(Option<usize>, Option<String>, Map<String, Value>)>, String> {
    value
        .as_array()
        .ok_or_else(|| "axis layout must be an array".to_string())?
        .iter()
        .map(|value| Ok((None, None, signed_field_spec(value)?)))
        .collect()
}

fn parse_object_specs(
    value: &Value,
) -> Result<Vec<(Option<usize>, Option<String>, Map<String, Value>)>, String> {
    value
        .as_array()
        .ok_or_else(|| "PivotTable repeated collection must be an array".to_string())?
        .iter()
        .map(|value| normalize_repeated_spec(value, &["sourceIndex", "rawXml", "attributes"]))
        .collect()
}

fn current_axis_indices(xml: &str, container_name: &str) -> Result<Vec<i64>, String> {
    let document = Document::parse(xml).map_err(|error| format!("PivotTable XML: {error}"))?;
    Ok(parse_axis_fields(document.root_element(), container_name))
}

fn page_field_indices(xml: &str) -> Result<Vec<i64>, String> {
    let document = Document::parse(xml).map_err(|error| format!("PivotTable XML: {error}"))?;
    direct_child(document.root_element(), "pageFields")
        .into_iter()
        .flat_map(|container| direct_children(container, "pageField"))
        .map(|field| {
            field
                .attribute("fld")
                .ok_or_else(|| "pageField has no fld attribute".to_string())?
                .parse::<i64>()
                .map_err(|error| format!("pageField fld: {error}"))
        })
        .collect()
}

fn data_field_indices(xml: &str) -> Result<Vec<i64>, String> {
    let document = Document::parse(xml).map_err(|error| format!("PivotTable XML: {error}"))?;
    direct_child(document.root_element(), "dataFields")
        .into_iter()
        .flat_map(|container| direct_children(container, "dataField"))
        .map(|field| {
            field
                .attribute("fld")
                .ok_or_else(|| "dataField has no fld attribute".to_string())?
                .parse::<i64>()
                .map_err(|error| format!("dataField fld: {error}"))
        })
        .collect()
}

fn validate_axis_layout(
    xml: &str,
    rows: &[i64],
    columns: &[i64],
    pages: &[i64],
    data: &[i64],
) -> Result<(), String> {
    let document = Document::parse(xml).map_err(|error| format!("PivotTable XML: {error}"))?;
    let field_count = direct_child(document.root_element(), "pivotFields")
        .map(|container| direct_children(container, "pivotField").count())
        .unwrap_or(0);
    let validate = |axis: &str, fields: &[i64], allow_values_pseudo_field: bool| {
        let mut seen = HashSet::new();
        for field in fields {
            if *field == -2 && allow_values_pseudo_field {
                if !seen.insert(*field) {
                    return Err(format!("{axis} axis contains field -2 more than once"));
                }
                continue;
            }
            if *field < 0
                || usize::try_from(*field)
                    .ok()
                    .is_none_or(|field| field >= field_count)
            {
                return Err(format!(
                    "{axis} axis field {field} is outside the {field_count} PivotFields"
                ));
            }
            if !seen.insert(*field) && axis != "data" {
                return Err(format!("{axis} axis contains field {field} more than once"));
            }
        }
        Ok(())
    };
    validate("row", rows, true)?;
    validate("column", columns, true)?;
    validate("page", pages, false)?;
    validate("data", data, false)?;
    let row: HashSet<_> = rows.iter().copied().collect();
    let column: HashSet<_> = columns.iter().copied().collect();
    let page: HashSet<_> = pages.iter().copied().collect();
    if let Some(field) = row.intersection(&column).next() {
        return Err(format!(
            "field {field} cannot be on both row and column axes"
        ));
    }
    if let Some(field) = row.intersection(&page).next() {
        return Err(format!("field {field} cannot be on both row and page axes"));
    }
    if let Some(field) = column.intersection(&page).next() {
        return Err(format!(
            "field {field} cannot be on both column and page axes"
        ));
    }
    Ok(())
}

fn synchronize_field_layout(
    xml: &str,
    rows: &[i64],
    columns: &[i64],
    pages: &[i64],
    data: &[i64],
) -> Result<String, String> {
    let document = Document::parse(xml).map_err(|error| format!("PivotTable XML: {error}"))?;
    let root = document.root_element();
    let Some(container) = direct_child(root, "pivotFields") else {
        return Ok(xml.to_string());
    };
    let fields: Vec<_> = direct_children(container, "pivotField").collect();
    let mut replacements = Vec::new();
    for (index, field) in fields.iter().enumerate() {
        let index = index as i64;
        let axis = if rows.contains(&index) {
            Some("axisRow")
        } else if columns.contains(&index) {
            Some("axisCol")
        } else if pages.contains(&index) {
            Some("axisPage")
        } else {
            None
        };
        let mut patch = Map::new();
        patch.insert(
            "axis".to_string(),
            axis.map(Value::from).unwrap_or(Value::Null),
        );
        patch.insert(
            "dataField".to_string(),
            if data.contains(&index) {
                Value::Bool(true)
            } else {
                Value::Null
            },
        );
        let range = field.range();
        let edited = patch_element_attributes(
            &xml[range.clone()],
            "pivotField",
            &patch,
            &pivot_field_rules(),
        )?;
        if edited != xml[range.clone()] {
            replacements.push((range, edited));
        }
    }
    apply_ranges(xml, replacements)
}

fn apply_axes(xml: &str, axes: &Map<String, Value>) -> Result<String, String> {
    for key in axes.keys() {
        if !["rows", "columns", "pages", "data"].contains(&key.as_str()) {
            return Err(format!("unsupported PivotTable axes key {key}"));
        }
    }
    let mut result = xml.to_string();
    if let Some(rows) = axes.get("rows") {
        let specs = parse_axis_specs(rows)?;
        result =
            rewrite_repeated_container(&result, "rowFields", "field", &specs, &field_node_rules())?;
    }
    if let Some(columns) = axes.get("columns") {
        let specs = parse_axis_specs(columns)?;
        result =
            rewrite_repeated_container(&result, "colFields", "field", &specs, &field_node_rules())?;
    }
    if let Some(pages) = axes.get("pages") {
        let specs = parse_object_specs(pages)?;
        result = rewrite_repeated_container(
            &result,
            "pageFields",
            "pageField",
            &specs,
            &rules(
                &[],
                PAGE_FIELD_SIGNED_ATTRIBUTES,
                &[],
                PAGE_FIELD_STRING_ATTRIBUTES,
            ),
        )?;
    }
    if let Some(data) = axes.get("data") {
        let specs = parse_object_specs(data)?;
        result = rewrite_repeated_container(
            &result,
            "dataFields",
            "dataField",
            &specs,
            &data_field_rules(),
        )?;
    }
    let rows = current_axis_indices(&result, "rowFields")?;
    let columns = current_axis_indices(&result, "colFields")?;
    let pages = page_field_indices(&result)?;
    let data = data_field_indices(&result)?;
    validate_axis_layout(&result, &rows, &columns, &pages, &data)?;
    synchronize_field_layout(&result, &rows, &columns, &pages, &data)
}

fn apply_filters(xml: &str, value: &Value) -> Result<String, String> {
    let specs = parse_object_specs(value)?;
    rewrite_repeated_container(xml, "filters", "filter", &specs, &filter_rules())
}

/// Apply a typed, differential edit to one native `pivotTableDefinition` XML part.
///
/// Supported top-level members:
/// - `display`: root display/printing/layout attributes;
/// - `location`: output range and first row/column offsets;
/// - `style`: `pivotTableStyleInfo` attributes;
/// - `fields`: field options, sorting, subtotal flags, and item visibility;
/// - `axes`: row/column/page/data field layout (including data aggregation/show-as options);
/// - `filters`: stable `sourceIndex` based filter edits/reordering; `rawXml` permits a validated
///   new native filter while existing opaque `autoFilter` children are retained.
///
/// Omitted values are untouched and JSON `null` removes an explicit attribute.  An empty patch
/// and semantically equal edits are byte-exact no-ops.
pub(crate) fn apply_pivot_table_patch(table_xml: &str, patch: &Value) -> Result<String, String> {
    let _ = root_open_tag_range_for(table_xml, "pivotTableDefinition")?;
    let object = patch
        .as_object()
        .ok_or_else(|| "PivotTable patch must be an object".to_string())?;
    for key in object.keys() {
        if !["display", "location", "style", "fields", "axes", "filters"].contains(&key.as_str()) {
            return Err(format!("unsupported PivotTable patch key {key}"));
        }
    }
    let mut result = table_xml.to_string();
    if let Some(display) = object_field(object, "display")? {
        result = patch_element_attributes(&result, "pivotTableDefinition", display, &root_rules())?;
    }
    if let Some(location) = object_field(object, "location")? {
        result = patch_direct_child(
            &result,
            "location",
            location,
            &rules(&[], &[], LOCATION_UNSIGNED_ATTRIBUTES, &["ref"]),
        )?;
    }
    if let Some(style) = object_field(object, "style")? {
        result = patch_direct_child(
            &result,
            "pivotTableStyleInfo",
            style,
            &rules(STYLE_BOOLEAN_ATTRIBUTES, &[], &[], STYLE_STRING_ATTRIBUTES),
        )?;
    }
    if let Some(fields) = object.get("fields") {
        let fields = fields
            .as_array()
            .ok_or_else(|| "PivotTable patch.fields must be an array".to_string())?;
        result = patch_pivot_fields(&result, fields)?;
    }
    if let Some(axes) = object_field(object, "axes")? {
        result = apply_axes(&result, axes)?;
    }
    if let Some(filters) = object.get("filters") {
        result = apply_filters(&result, filters)?;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture_parts() -> BTreeMap<String, Vec<u8>> {
        [
            (
                "_rels/.rels",
                br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="office-z" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/custom-book.xml"/></Relationships>"#.as_slice(),
            ),
            (
                "xl/custom-book.xml",
                br#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Report" sheetId="7" r:id="sheet-rel-weird"/></sheets></workbook>"#.as_slice(),
            ),
            (
                "xl/_rels/custom-book.xml.rels",
                br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="sheet-rel-weird" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="sheets/report-data.xml"/></Relationships>"#.as_slice(),
            ),
            (
                "xl/sheets/report-data.xml",
                br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheetData/><pivotTableParts count="1"><pivotTablePart r:id="pt-any-id"/></pivotTableParts></worksheet>"#.as_slice(),
            ),
            (
                "xl/sheets/_rels/report-data.xml.rels",
                br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="pt-any-id" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotTable" Target="../objects/native-report.xml"/></Relationships>"#.as_slice(),
            ),
            (
                "xl/objects/native-report.xml",
                br#"<?xml version="1.0" encoding="UTF-8"?><pivotTableDefinition xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:x15="http://schemas.microsoft.com/office/spreadsheetml/2010/11/main" name="SalesPivot" cacheId="73" compact="1" outline="0" rowGrandTotals="1" colGrandTotals="1" showHeaders="true" preserveFormatting="1" multipleFieldFilters="0" vendorRoot="KEEP"><location ref="A3:F20" firstHeaderRow="1" firstDataRow="1" firstDataCol="1" vendorLocation="KEEP"/><pivotFields count="4"><pivotField axis="axisRow" showAll="1" defaultSubtotal="1" vendorField="A"><items count="2"><item x="0"/><item x="1" h="1" vendorItem="KEEP"/></items><extLst><ext uri="field-opaque"><x15:future keep="yes"/></ext></extLst></pivotField><pivotField axis="axisCol" sortType="ascending" dataSourceSort="1" rankBy="2" showAll="0"/><pivotField dataField="1" sumSubtotal="1"/><pivotField axis="axisPage" multipleItemSelectionAllowed="1"/></pivotFields><rowFields count="1"><field x="0" vendorAxis="KEEP"/></rowFields><colFields count="1"><field x="1"/></colFields><pageFields count="1"><pageField fld="3" item="0" hier="-1" name="Page caption" vendorPage="KEEP"/></pageFields><dataFields count="1"><dataField name="Sum of Amount" fld="2" subtotal="sum" showDataAs="normal" numFmtId="4" vendorData="KEEP"><extLst><ext uri="opaque-data"/></extLst></dataField></dataFields><pivotTableStyleInfo name="PivotStyleMedium9" showRowHeaders="1" showColHeaders="1" showRowStripes="0" vendorStyle="KEEP"/><filters count="1"><filter fld="1" type="captionContains" evalOrder="-1" id="19" stringValue1="east" vendorFilter="KEEP"><autoFilter ref="A1"><filterColumn colId="0"><customFilters><customFilter operator="contains" val="east"/></customFilters></filterColumn></autoFilter><extLst><x15:future/></extLst></filter></filters><extLst><ext uri="root-opaque"><x15:payload answer="42"/></ext></extLst></pivotTableDefinition>"#.as_slice(),
            ),
            (
                "xl/objects/_rels/native-report.xml.rels",
                br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="cache-link-arbitrary" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotCacheDefinition" Target="../cache/nonstandard-cache.xml"/></Relationships>"#.as_slice(),
            ),
            (
                "xl/cache/nonstandard-cache.xml",
                br#"<pivotCacheDefinition xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><cacheSource type="worksheet"><worksheetSource sheet="Data" ref="A1:D99"/></cacheSource><cacheFields count="4"><cacheField name="Region"><sharedItems count="2" containsString="1"><s v="East"/><s v="West"/></sharedItems></cacheField><cacheField name="Month"><sharedItems count="1" containsDate="1"><d v="2026-08-05T00:00:00"/></sharedItems></cacheField><cacheField name="Amount"><sharedItems count="2" containsNumber="1"><n v="42.5"/><m/></sharedItems></cacheField><cacheField name="Channel"/></cacheFields></pivotCacheDefinition>"#.as_slice(),
            ),
            (
                "xl/objects/detached-any-name.xml",
                br#"<pivotTableDefinition xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" name="Detached" cacheId="99"><location ref="J2:K3"/></pivotTableDefinition>"#.as_slice(),
            ),
        ]
        .into_iter()
        .map(|(name, bytes)| (name.to_string(), bytes.to_vec()))
        .collect()
    }

    fn table_xml() -> String {
        String::from_utf8(fixture_parts()["xl/objects/native-report.xml"].clone()).unwrap()
    }

    #[test]
    fn relationship_graph_and_model_cover_fields_layout_filter_and_cache_names() {
        let parts = fixture_parts();
        let (workbook, resolved) = resolve_pivot_table_parts(&parts).unwrap();
        assert_eq!(workbook, "xl/custom-book.xml");
        assert_eq!(resolved.len(), 2);
        let live = resolved.iter().find(|table| table.sheet.is_some()).unwrap();
        assert_eq!(live.part, "xl/objects/native-report.xml");
        assert_eq!(live.sheet.as_deref(), Some("Report"));
        assert_eq!(live.relationship_id.as_deref(), Some("pt-any-id"));
        assert_eq!(
            live.cache_part.as_deref(),
            Some("xl/cache/nonstandard-cache.xml")
        );
        assert!(
            resolved
                .iter()
                .any(|table| table.part.ends_with("detached-any-name.xml"))
        );

        let model = parse_pivot_table_model(&parts).unwrap();
        let tables = model["tables"].as_array().unwrap();
        let table = tables
            .iter()
            .find(|table| table["sheet"] == "Report")
            .unwrap();
        assert_eq!(table["name"], "SalesPivot");
        assert_eq!(table["cacheId"], 73);
        assert_eq!(table["location"]["ref"], "A3:F20");
        assert_eq!(table["display"]["compact"], true);
        assert_eq!(table["fields"][0]["name"], "Region");
        assert_eq!(table["fields"][0]["items"][0]["cacheIndex"], 0);
        assert_eq!(table["fields"][0]["items"][0]["label"], "East");
        assert_eq!(table["fields"][0]["items"][0]["value"], "East");
        assert_eq!(table["fields"][0]["items"][1]["label"], "West");
        assert_eq!(table["fields"][1]["sort"]["sortType"], "ascending");
        assert_eq!(table["fields"][0]["items"][1]["attributes"]["h"], true);
        assert_eq!(table["axes"]["rows"][0], 0);
        assert_eq!(table["axes"]["data"][0]["subtotal"], "sum");
        assert_eq!(table["filters"][0]["attributes"]["type"], "captionContains");
        assert_eq!(table["filters"][0]["attributes"]["evalOrder"], -1);
    }

    #[test]
    fn empty_and_semantically_equal_patch_is_byte_exact() {
        let original = table_xml();
        assert_eq!(
            apply_pivot_table_patch(&original, &json!({})).unwrap(),
            original
        );
        let edited = apply_pivot_table_patch(
            &original,
            &json!({
                "display": {"compact": true, "outline": false, "showHeaders": true},
                "location": {"ref": "A3:F20", "firstHeaderRow": 1},
                "style": {"name": "PivotStyleMedium9", "showRowHeaders": true},
                "fields": [{
                    "index": 1,
                    "sort": {"sortType": "ascending", "dataSourceSort": true, "rankBy": 2}
                }]
            }),
        )
        .unwrap();
        assert_eq!(edited, original);
        let layout_no_op = apply_pivot_table_patch(
            &original,
            &json!({
                "axes": {
                    "rows": [0],
                    "columns": [1],
                    "pages": [{"sourceIndex": 0, "fld": 3, "item": 0, "hier": -1, "name": "Page caption"}],
                    "data": [{"sourceIndex": 0, "name": "Sum of Amount", "fld": 2, "subtotal": "sum", "showDataAs": "normal", "numFmtId": 4}]
                }
            }),
        )
        .unwrap();
        assert_eq!(layout_no_op, original);
    }

    #[test]
    fn display_location_style_and_unknown_extensions_are_preserved() {
        let original = table_xml();
        let edited = apply_pivot_table_patch(
            &original,
            &json!({
                "display": {
                    "compact": false,
                    "outline": true,
                    "showEmptyRow": true,
                    "grandTotalCaption": "All & Total",
                    "colGrandTotals": null
                },
                "location": {"ref": "B4:H30", "firstDataCol": 2},
                "style": {"name": "PivotStyleLight16", "showRowStripes": true}
            }),
        )
        .unwrap();
        assert!(edited.contains("compact=\"0\""));
        assert!(edited.contains("outline=\"1\""));
        assert!(edited.contains("showEmptyRow=\"1\""));
        assert!(edited.contains("grandTotalCaption=\"All &amp; Total\""));
        assert!(
            !root_open_tag_range_for(&edited, "pivotTableDefinition")
                .map(|range| edited[range].contains("colGrandTotals="))
                .unwrap()
        );
        assert!(edited.contains("ref=\"B4:H30\""));
        assert!(edited.contains("firstDataCol=\"2\""));
        assert!(edited.contains("vendorRoot=\"KEEP\""));
        assert!(edited.contains("vendorLocation=\"KEEP\""));
        assert!(edited.contains("vendorStyle=\"KEEP\""));
        assert!(edited.contains("<x15:payload answer=\"42\"/>"));
    }

    #[test]
    fn field_sort_subtotals_and_manual_item_filter_are_differential() {
        let original = table_xml();
        let edited = apply_pivot_table_patch(
            &original,
            &json!({
                "fields": [
                    {
                        "index": 0,
                        "attributes": {"compact": false, "showAll": false},
                        "subtotals": {"defaultSubtotal": false, "sumSubtotal": true},
                        "hiddenItems": [0],
                        "items": [{"sourceIndex": 1, "attributes": {"n": "Other"}}]
                    },
                    {
                        "index": 1,
                        "sort": {"sortType": "descending", "dataSourceSort": false, "rankBy": 3}
                    }
                ]
            }),
        )
        .unwrap();
        let document = Document::parse(&edited).unwrap();
        let fields: Vec<_> = direct_children(
            direct_child(document.root_element(), "pivotFields").unwrap(),
            "pivotField",
        )
        .collect();
        assert_eq!(
            parse_bool(fields[0].attribute("compact").unwrap()),
            Some(false)
        );
        assert_eq!(
            parse_bool(fields[0].attribute("showAll").unwrap()),
            Some(false)
        );
        assert_eq!(
            parse_bool(fields[0].attribute("defaultSubtotal").unwrap()),
            Some(false)
        );
        assert_eq!(
            parse_bool(fields[0].attribute("sumSubtotal").unwrap()),
            Some(true)
        );
        let items: Vec<_> =
            direct_children(direct_child(fields[0], "items").unwrap(), "item").collect();
        assert_eq!(parse_bool(items[0].attribute("h").unwrap()), Some(true));
        assert_eq!(items[1].attribute("h"), None);
        assert_eq!(items[1].attribute("n"), Some("Other"));
        assert_eq!(fields[1].attribute("sortType"), Some("descending"));
        assert_eq!(
            parse_bool(fields[1].attribute("dataSourceSort").unwrap()),
            Some(false)
        );
        assert_eq!(fields[1].attribute("rankBy"), Some("3"));
        assert!(edited.contains("vendorField=\"A\""));
        assert!(edited.contains("vendorItem=\"KEEP\""));
        assert!(edited.contains("<x15:future keep=\"yes\"/>"));
    }

    #[test]
    fn axes_reorder_insert_remove_and_data_aggregation_preserve_opaque_nodes() {
        let original = table_xml();
        let edited = apply_pivot_table_patch(
            &original,
            &json!({
                "axes": {
                    "rows": [1, 0],
                    "columns": [],
                    "pages": [{"sourceIndex": 0, "fld": 3, "item": 1, "hier": -1}],
                    "data": [
                        {"sourceIndex": 0, "fld": 2, "name": "Average Amount", "subtotal": "average", "showDataAs": "percentOfTotal", "numFmtId": 10},
                        {"fld": 2, "name": "Count Amount", "subtotal": "count", "showDataAs": "normal"}
                    ]
                }
            }),
        )
        .unwrap();
        let document = Document::parse(&edited).unwrap();
        let root = document.root_element();
        assert_eq!(parse_axis_fields(root, "rowFields"), vec![1, 0]);
        assert!(direct_child(root, "colFields").is_none());
        assert_eq!(parse_axis_fields(root, "colFields"), Vec::<i64>::new());
        let data: Vec<_> =
            direct_children(direct_child(root, "dataFields").unwrap(), "dataField").collect();
        assert_eq!(data.len(), 2);
        assert_eq!(data[0].attribute("subtotal"), Some("average"));
        assert_eq!(data[0].attribute("showDataAs"), Some("percentOfTotal"));
        assert_eq!(data[1].attribute("subtotal"), Some("count"));
        assert!(edited.contains("vendorData=\"KEEP\""));
        assert!(edited.contains("<ext uri=\"opaque-data\"/>"));
        assert!(edited.contains("vendorAxis=\"KEEP\""));
        assert!(edited.contains("vendorPage=\"KEEP\""));
        let fields: Vec<_> =
            direct_children(direct_child(root, "pivotFields").unwrap(), "pivotField").collect();
        assert_eq!(fields[0].attribute("axis"), Some("axisRow"));
        assert_eq!(fields[1].attribute("axis"), Some("axisRow"));
        assert_eq!(fields[2].attribute("axis"), None);
        assert_eq!(
            parse_bool(fields[2].attribute("dataField").unwrap()),
            Some(true)
        );
    }

    #[test]
    fn filter_edit_and_validated_native_add_preserve_auto_filter_and_extension() {
        let original = table_xml();
        let edited = apply_pivot_table_patch(
            &original,
            &json!({
                "filters": [
                    {"sourceIndex": 0, "type": "captionNotContains", "stringValue1": "west", "evalOrder": 2},
                    {"rawXml": "<filter xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" fld=\"2\" type=\"valueGreaterThan\"><autoFilter ref=\"A1\"/></filter>", "id": 20, "evalOrder": 3}
                ]
            }),
        )
        .unwrap();
        let document = Document::parse(&edited).unwrap();
        let filters: Vec<_> = direct_children(
            direct_child(document.root_element(), "filters").unwrap(),
            "filter",
        )
        .collect();
        assert_eq!(filters.len(), 2);
        assert_eq!(filters[0].attribute("type"), Some("captionNotContains"));
        assert_eq!(filters[0].attribute("stringValue1"), Some("west"));
        assert_eq!(filters[0].attribute("evalOrder"), Some("2"));
        assert_eq!(filters[1].attribute("type"), Some("valueGreaterThan"));
        assert_eq!(filters[1].attribute("id"), Some("20"));
        assert!(edited.contains("vendorFilter=\"KEEP\""));
        assert!(edited.contains("<customFilter operator=\"contains\" val=\"east\"/>"));
        assert!(edited.contains("<x15:future/>"));
    }

    #[test]
    fn invalid_types_enums_indices_and_unknown_keys_are_rejected() {
        let original = table_xml();
        assert!(apply_pivot_table_patch(&original, &json!({"mystery": {}})).is_err());
        assert!(
            apply_pivot_table_patch(&original, &json!({"display": {"compact": "yes"}})).is_err()
        );
        assert!(
            apply_pivot_table_patch(
                &original,
                &json!({"fields": [{"index": 99, "sort": {"sortType": "ascending"}}]})
            )
            .is_err()
        );
        assert!(
            apply_pivot_table_patch(
                &original,
                &json!({"fields": [{"index": 0, "sort": {"sortType": "sideways"}}]})
            )
            .is_err()
        );
        assert!(
            apply_pivot_table_patch(&original, &json!({"axes": {"rows": [99], "unknown": []}}))
                .is_err()
        );
        assert!(
            apply_pivot_table_patch(&original, &json!({"filters": [{"sourceIndex": 7}]})).is_err()
        );
        assert!(
            apply_pivot_table_patch(&original, &json!({"filters": [{"rawXml": "<wrong/>"}]}))
                .is_err()
        );
    }
}
