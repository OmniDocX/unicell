//! Loss-minimising editor for the data part of an OOXML SmartArt diagram.
//!
//! The module deliberately edits only the `dgm:ptLst` and `dgm:cxnLst` records
//! that describe the logical tree. Existing point/connection XML is patched in
//! place so unsupported attributes, rich-text formatting and extension subtrees
//! survive. Presentation payload is otherwise opaque; logical insertions clone a
//! nearby presentation mapping (or create the schema-minimal mapping), and logical
//! deletions remove only presentation points exclusively owned by deleted nodes.

use roxmltree::Node;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::ops::Range;

const DGM_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/diagram";
const A_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";

#[derive(Clone, Debug)]
struct TextElement {
    range: Range<usize>,
}

#[derive(Clone, Debug)]
struct PointRecord {
    id: String,
    kind: String,
    text: String,
    paragraphs: Value,
    range: Range<usize>,
    text_elements: Vec<TextElement>,
    text_container: Option<Range<usize>>,
    presentation_assoc_id: Option<String>,
    presentation_name: Option<String>,
    presentation_style_count: Option<i64>,
    document_order: usize,
    editable: bool,
}

#[derive(Clone, Debug)]
struct ConnectionRecord {
    id: Option<String>,
    kind: String,
    src_id: String,
    dest_id: String,
    src_order: Option<i64>,
    dest_order: Option<i64>,
    sibling_transition_id: Option<String>,
    range: Range<usize>,
    hierarchy: bool,
    document_order: usize,
}

#[derive(Clone, Debug)]
struct ParsedSmartArt {
    layout: Option<String>,
    points: Vec<PointRecord>,
    connections: Vec<ConnectionRecord>,
    point_list_range: Range<usize>,
    point_list_insert: Option<usize>,
    connection_list_range: Option<Range<usize>>,
    connection_list_insert: Option<usize>,
}

#[derive(Clone, Debug)]
struct NewHierarchyIds {
    connection_id: String,
    par_transition_id: String,
    sibling_transition_id: String,
}

#[derive(Clone, Debug)]
struct ModelNode {
    id: String,
    parent_id: Option<String>,
    text: String,
    paragraphs: Value,
    order: i64,
    kind: String,
    document_order: usize,
}

#[derive(Clone, Debug)]
struct DesiredNode {
    id: String,
    parent_id: Option<String>,
    text: String,
    paragraphs: Value,
    order: i64,
    kind: String,
    document_order: usize,
    original: Option<ModelNode>,
}

#[derive(Clone, Debug)]
enum ParentEdit {
    Keep,
    Set(Option<String>),
}

#[derive(Clone, Debug)]
struct NodeEdit {
    key: Option<String>,
    text: Option<String>,
    paragraphs: Option<Value>,
    parent: ParentEdit,
    order: Option<i64>,
    kind: Option<String>,
}

#[derive(Clone, Debug)]
struct Patch {
    range: Range<usize>,
    replacement: String,
}

/// Parses the editable logical SmartArt tree from a diagram data XML part.
///
/// The returned object has the stable shape
/// `{ "layout": string|null, "nodes": [{id,parentId,text,paragraphs,order,kind}] }`.
/// Presentation/transition points remain in the XML but are intentionally not
/// surfaced as logical nodes.
pub fn parse_smartart_model(data_xml: &str) -> Value {
    match parse_document(data_xml) {
        Ok(parsed) => {
            let nodes = build_model_nodes(&parsed);
            json!({
                "layout": parsed.layout,
                "nodes": nodes.into_iter().map(|node| json!({
                    "id": node.id,
                    "parentId": node.parent_id,
                    "text": node.text,
                    "paragraphs": node.paragraphs,
                    "order": node.order,
                    "kind": node.kind,
                })).collect::<Vec<_>>(),
            })
        }
        Err(error) => json!({ "layout": Value::Null, "nodes": [], "error": error }),
    }
}

/// Applies logical SmartArt edits while retaining unknown OOXML payload.
///
/// Two edit shapes are accepted:
///
/// * `{ "nodes": [...] }` is a complete replacement of the logical node list.
/// * Operation form accepts `addNodes`, `updates`/`moveNodes`, and
///   `deleteIds`/`deleteNodes`. Deletion cascades by default; set
///   `"cascade": false` to reparent surviving children to the deleted node's
///   former parent.
///
/// A node not already present is always assigned a fresh, collision-free GUID.
/// Its supplied `id` acts as a temporary key so another new node may use it as
/// `parentId` in the same edit.
pub fn apply_smartart_edit(data_xml: &str, edit: &Value) -> Result<String, String> {
    let parsed = parse_document(data_xml)?;
    let edit_obj = edit
        .as_object()
        .ok_or("SmartArt edit must be a JSON object")?;
    let current_nodes = build_model_nodes(&parsed);
    let current: HashMap<String, ModelNode> = current_nodes
        .iter()
        .cloned()
        .map(|node| (node.id.clone(), node))
        .collect();

    let mut used_ids = collect_model_ids(data_xml)?;
    let mut guid_factory = GuidFactory::new(data_xml, edit, &used_ids);
    let mut temp_ids = HashMap::<String, String>::new();
    let full_replacement = edit_obj.contains_key("nodes");

    let mut desired: HashMap<String, DesiredNode> = if full_replacement {
        HashMap::new()
    } else {
        current
            .values()
            .cloned()
            .map(|node| {
                let id = node.id.clone();
                (
                    id.clone(),
                    DesiredNode {
                        id,
                        parent_id: node.parent_id.clone(),
                        text: node.text.clone(),
                        paragraphs: node.paragraphs.clone(),
                        order: node.order,
                        kind: node.kind.clone(),
                        document_order: node.document_order,
                        original: Some(node),
                    },
                )
            })
            .collect()
    };

    if full_replacement {
        let specs = parse_node_array(edit_obj.get("nodes").unwrap(), "nodes")?;
        assign_new_ids(
            &specs,
            &current,
            false,
            &mut temp_ids,
            &mut used_ids,
            &mut guid_factory,
        )?;
        for (index, spec) in specs.iter().enumerate() {
            upsert_spec(spec, index, &current, &temp_ids, &mut desired, false)?;
        }
    } else {
        let add_specs = collect_node_arrays(edit_obj, &["addNodes", "add"])?;
        assign_new_ids(
            &add_specs,
            &current,
            true,
            &mut temp_ids,
            &mut used_ids,
            &mut guid_factory,
        )?;
        for (index, spec) in add_specs.iter().enumerate() {
            upsert_spec(spec, index, &current, &temp_ids, &mut desired, true)?;
        }

        let update_specs =
            collect_node_arrays(edit_obj, &["updates", "updateNodes", "moveNodes", "moves"])?;
        for (index, spec) in update_specs.iter().enumerate() {
            upsert_spec(spec, index, &current, &temp_ids, &mut desired, false)?;
        }
        if edit_obj.contains_key("id") {
            let spec = parse_node_edit(edit)?;
            upsert_spec(&spec, 0, &current, &temp_ids, &mut desired, false)?;
        }

        let explicit_deletes = collect_delete_ids(edit_obj)?;
        if !explicit_deletes.is_empty() {
            apply_deletions(
                &explicit_deletes,
                edit_obj
                    .get("cascade")
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
                &current,
                &mut desired,
            )?;
        }
    }

    resolve_parent_temp_ids(&temp_ids, &mut desired);
    validate_tree(&parsed, &desired)?;
    normalize_changed_sibling_orders(&current, &mut desired);

    let deleted: HashSet<String> = current
        .keys()
        .filter(|id| !desired.contains_key(*id))
        .cloned()
        .collect();
    let deleted_presentation = exclusively_deleted_presentation_points(&parsed, &deleted);
    let mut patches = Vec::<Patch>::new();

    for point in &parsed.points {
        if !point.editable {
            if deleted_presentation.contains(&point.id) {
                patches.push(Patch {
                    range: point.range.clone(),
                    replacement: String::new(),
                });
            }
            continue;
        }
        let Some(node) = desired.get(&point.id) else {
            patches.push(Patch {
                range: point.range.clone(),
                replacement: String::new(),
            });
            continue;
        };
        let original = node.original.as_ref();
        let text_changed = original.map(|v| v.text.as_str()) != Some(node.text.as_str());
        let paragraphs_changed = original.map(|value| &value.paragraphs) != Some(&node.paragraphs);
        let kind_changed = original.map(|v| v.kind.as_str()) != Some(node.kind.as_str());
        if text_changed || paragraphs_changed || kind_changed {
            let replacement = edit_existing_point(
                data_xml,
                point,
                node,
                text_changed,
                paragraphs_changed,
                kind_changed,
            )?;
            patches.push(Patch {
                range: point.range.clone(),
                replacement,
            });
        }
    }

    let mut new_nodes: Vec<&DesiredNode> = desired
        .values()
        .filter(|node| node.original.is_none())
        .collect();
    new_nodes.sort_by(|a, b| {
        a.parent_id
            .cmp(&b.parent_id)
            .then(a.order.cmp(&b.order))
            .then(a.document_order.cmp(&b.document_order))
            .then(a.id.cmp(&b.id))
    });
    let logical_point_xml: String = new_nodes.iter().map(|node| new_point_xml(node)).collect();

    let mut hierarchy_by_child: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, connection) in parsed.connections.iter().enumerate() {
        if connection.hierarchy {
            hierarchy_by_child
                .entry(connection.dest_id.clone())
                .or_default()
                .push(index);
        }
    }
    let mut new_hierarchy_ids = HashMap::<String, NewHierarchyIds>::new();
    let mut new_transition_points = String::new();
    let mut new_hierarchy_connections = String::new();
    for node in &new_nodes {
        let Some(parent_id) = node.parent_id.as_deref() else {
            continue;
        };
        if hierarchy_by_child
            .get(&node.id)
            .and_then(|items| items.first())
            .is_some()
        {
            continue;
        }
        let ids = NewHierarchyIds {
            connection_id: guid_factory.next(&mut used_ids),
            par_transition_id: guid_factory.next(&mut used_ids),
            sibling_transition_id: guid_factory.next(&mut used_ids),
        };
        new_transition_points.push_str(&transition_point_xml(
            &ids.par_transition_id,
            "parTrans",
            &ids.connection_id,
        ));
        new_transition_points.push_str(&transition_point_xml(
            &ids.sibling_transition_id,
            "sibTrans",
            &ids.connection_id,
        ));
        new_hierarchy_connections.push_str(&hierarchy_connection_xml(
            &ids.connection_id,
            parent_id,
            &node.id,
            node.order,
            &ids.par_transition_id,
            &ids.sibling_transition_id,
        ));
        new_hierarchy_ids.insert(node.id.clone(), ids);
    }

    let mut sibling_transition_by_child = HashMap::<String, String>::new();
    for connection in parsed
        .connections
        .iter()
        .filter(|connection| connection.hierarchy)
    {
        if let Some(id) = &connection.sibling_transition_id {
            sibling_transition_by_child
                .entry(connection.dest_id.clone())
                .or_insert_with(|| id.clone());
        }
    }
    for (node_id, ids) in &new_hierarchy_ids {
        sibling_transition_by_child.insert(node_id.clone(), ids.sibling_transition_id.clone());
    }

    let presentation_points: HashMap<String, &PointRecord> = parsed
        .points
        .iter()
        .filter(|point| point.kind == "pres")
        .map(|point| (point.id.clone(), point))
        .collect();
    let mut presentation_mappings = HashMap::<String, Vec<usize>>::new();
    for (index, connection) in parsed.connections.iter().enumerate() {
        if connection.kind == "presOf" && presentation_points.contains_key(&connection.dest_id) {
            presentation_mappings
                .entry(connection.src_id.clone())
                .or_default()
                .push(index);
        }
    }
    let mapped_sources: HashSet<String> = presentation_mappings.keys().cloned().collect();
    let mut used_presentation_dest_orders = HashMap::<String, HashSet<i64>>::new();
    for connection in parsed.connections.iter().filter(|connection| {
        connection.kind == "presOf"
            && !deleted.contains(&connection.src_id)
            && !deleted_presentation.contains(&connection.dest_id)
    }) {
        used_presentation_dest_orders
            .entry(connection.dest_id.clone())
            .or_default()
            .insert(connection.dest_order.unwrap_or(0));
    }

    // Office's list-style root presentation is an alternating sequence of node and
    // sibling-transition presentation points. Reconcile its style counters and source
    // orders whenever a direct child is inserted beneath the document point.
    let mut office_root_style_counts = HashMap::<String, i64>::new();
    for node in &new_nodes {
        let Some(parent_id) = node.parent_id.as_deref() else {
            continue;
        };
        if desired
            .get(parent_id)
            .is_none_or(|parent| parent.kind != "doc")
        {
            continue;
        }
        let has_office_style = presentation_points.values().any(|point| {
            point.kind == "pres"
                && point.presentation_name.as_deref() == Some("node")
                && point.presentation_style_count.is_some()
                && point
                    .presentation_assoc_id
                    .as_deref()
                    .and_then(|id| desired.get(id))
                    .is_some_and(|logical| logical.parent_id.as_deref() == Some(parent_id))
        });
        if has_office_style {
            let count = desired
                .values()
                .filter(|logical| logical.parent_id.as_deref() == Some(parent_id))
                .count() as i64;
            office_root_style_counts.insert(parent_id.to_string(), count);
        }
    }

    for point in presentation_points.values().filter(|point| {
        point.presentation_name.as_deref() == Some("node")
            && !deleted_presentation.contains(&point.id)
    }) {
        let Some(logical) = point
            .presentation_assoc_id
            .as_deref()
            .and_then(|id| desired.get(id))
        else {
            continue;
        };
        let Some(parent_id) = logical.parent_id.as_deref() else {
            continue;
        };
        let Some(style_count) = office_root_style_counts.get(parent_id) else {
            continue;
        };
        let mut replacement = data_xml[point.range.clone()].to_string();
        replacement = set_descendant_start_tag_attribute(
            &replacement,
            "prSet",
            "presStyleIdx",
            &logical.order.to_string(),
        );
        replacement = set_descendant_start_tag_attribute(
            &replacement,
            "prSet",
            "presStyleCnt",
            &style_count.to_string(),
        );
        if replacement != data_xml[point.range.clone()] {
            patches.push(Patch {
                range: point.range.clone(),
                replacement,
            });
        }
        for connection in parsed
            .connections
            .iter()
            .filter(|connection| connection.kind == "presParOf" && connection.dest_id == point.id)
        {
            let replacement = set_start_tag_attribute(
                &data_xml[connection.range.clone()],
                "srcOrd",
                &logical.order.saturating_mul(2).to_string(),
            );
            if replacement != data_xml[connection.range.clone()] {
                patches.push(Patch {
                    range: connection.range.clone(),
                    replacement,
                });
            }
        }
    }

    let sibling_presentation_template = presentation_points
        .values()
        .filter(|point| point.presentation_name.as_deref() == Some("sibTrans"))
        .min_by_key(|point| point.document_order)
        .copied();
    let mut new_sibling_presentation_points = String::new();
    let mut new_sibling_presentation_connections = String::new();
    let mut generated_sibling_associations = HashSet::<String>::new();
    for (parent_id, style_count) in &office_root_style_counts {
        let Some(parent_mapping) = presentation_mappings
            .get(parent_id)
            .and_then(|items| items.first())
            .map(|index| &parsed.connections[*index])
        else {
            continue;
        };
        let mut children: Vec<&DesiredNode> = desired
            .values()
            .filter(|logical| logical.parent_id.as_deref() == Some(parent_id.as_str()))
            .collect();
        children.sort_by(|left, right| {
            left.order
                .cmp(&right.order)
                .then(left.document_order.cmp(&right.document_order))
                .then(left.id.cmp(&right.id))
        });
        for (index, child) in children.iter().enumerate() {
            if index + 1 >= *style_count as usize {
                break;
            }
            let Some(transition_id) = sibling_transition_by_child.get(&child.id) else {
                continue;
            };
            if let Some(existing_point) = presentation_points.values().find(|point| {
                point.presentation_name.as_deref() == Some("sibTrans")
                    && point.presentation_assoc_id.as_deref() == Some(transition_id.as_str())
            }) {
                for connection in parsed.connections.iter().filter(|connection| {
                    connection.kind == "presParOf" && connection.dest_id == existing_point.id
                }) {
                    let replacement = set_start_tag_attribute(
                        &data_xml[connection.range.clone()],
                        "srcOrd",
                        &((index as i64).saturating_mul(2) + 1).to_string(),
                    );
                    if replacement != data_xml[connection.range.clone()] {
                        patches.push(Patch {
                            range: connection.range.clone(),
                            replacement,
                        });
                    }
                }
                continue;
            }
            if !generated_sibling_associations.insert(transition_id.clone()) {
                continue;
            }
            let point_id = guid_factory.next(&mut used_ids);
            new_sibling_presentation_points.push_str(&sibling_presentation_point_xml(
                data_xml,
                sibling_presentation_template,
                &point_id,
                transition_id,
            ));
            let connection_id = guid_factory.next(&mut used_ids);
            let template_connection = sibling_presentation_template.and_then(|template| {
                parsed.connections.iter().find(|connection| {
                    connection.kind == "presParOf" && connection.dest_id == template.id
                })
            });
            let source_order = (index as i64).saturating_mul(2) + 1;
            if let Some(template_connection) = template_connection {
                new_sibling_presentation_connections.push_str(&clone_connection_xml(
                    data_xml,
                    template_connection,
                    &connection_id,
                    &parent_mapping.dest_id,
                    &point_id,
                    Some(source_order),
                    Some(0),
                ));
            } else {
                new_sibling_presentation_connections.push_str(
                    &minimal_presentation_parent_connection_xml(
                        &connection_id,
                        &parent_mapping.dest_id,
                        &point_id,
                        source_order,
                    ),
                );
            }
        }
    }

    let mut new_presentation_points = String::new();
    let mut new_presentation_connections = String::new();
    for node in &new_nodes {
        let parent = node.parent_id.as_deref().and_then(|id| desired.get(id));
        let mut cloned_mapping = false;

        // Nested logical children share their parent's presentation target. This is the
        // structure Office itself writes: the separate owners are distinguished by destOrd.
        if parent.is_some_and(|parent| parent.kind != "doc") {
            if let Some(parent_mapping) = node
                .parent_id
                .as_deref()
                .and_then(|id| presentation_mappings.get(id))
                .and_then(|items| items.first())
                .map(|index| &parsed.connections[*index])
            {
                let used_orders = used_presentation_dest_orders
                    .entry(parent_mapping.dest_id.clone())
                    .or_default();
                let destination_order = (0i64..)
                    .find(|order| !used_orders.contains(order))
                    .unwrap_or(used_orders.len() as i64);
                used_orders.insert(destination_order);
                let connection_id = guid_factory.next(&mut used_ids);
                new_presentation_connections.push_str(&clone_connection_xml(
                    data_xml,
                    parent_mapping,
                    &connection_id,
                    &node.id,
                    &parent_mapping.dest_id,
                    Some(0),
                    Some(destination_order),
                ));
                cloned_mapping = true;
            }
        }

        let template_id = select_presentation_template(node, &current, &desired, &mapped_sources);
        if !cloned_mapping && let Some(template_id) = template_id {
            let mapping_indices = presentation_mappings
                .get(&template_id)
                .cloned()
                .unwrap_or_default();
            let mut point_id_map = HashMap::<String, String>::new();
            for mapping_index in &mapping_indices {
                let mapping = &parsed.connections[*mapping_index];
                let Some(template_point) = presentation_points.get(&mapping.dest_id) else {
                    continue;
                };
                if point_id_map.contains_key(&template_point.id) {
                    continue;
                }
                let new_point_id = guid_factory.next(&mut used_ids);
                let point_xml = node
                    .parent_id
                    .as_deref()
                    .and_then(|parent_id| office_root_style_counts.get(parent_id))
                    .filter(|_| template_point.presentation_name.as_deref() == Some("node"))
                    .map(|style_count| {
                        presentation_style_point_xml(
                            data_xml,
                            template_point,
                            &new_point_id,
                            &node.id,
                            node.order,
                            *style_count,
                        )
                    })
                    .unwrap_or_else(|| {
                        clone_presentation_point_xml(
                            data_xml,
                            template_point,
                            &new_point_id,
                            &template_id,
                            &node.id,
                        )
                    });
                new_presentation_points.push_str(&point_xml);
                point_id_map.insert(template_point.id.clone(), new_point_id);
            }

            for mapping_index in &mapping_indices {
                let mapping = &parsed.connections[*mapping_index];
                let Some(new_point_id) = point_id_map.get(&mapping.dest_id) else {
                    continue;
                };
                let connection_id = guid_factory.next(&mut used_ids);
                new_presentation_connections.push_str(&clone_connection_xml(
                    data_xml,
                    mapping,
                    &connection_id,
                    &node.id,
                    new_point_id,
                    None,
                    None,
                ));
                cloned_mapping = true;
            }

            // Clone the inbound/internal presentation hierarchy needed to place the new
            // presentation points. Outbound edges into another logical node's presentation
            // payload are intentionally not copied.
            if cloned_mapping {
                let template_order = current
                    .get(&template_id)
                    .map(|template| template.order)
                    .unwrap_or(node.order);
                let order_delta = node.order.saturating_sub(template_order);
                for connection in parsed
                    .connections
                    .iter()
                    .filter(|connection| connection.kind == "presParOf")
                {
                    let Some(new_destination) = point_id_map.get(&connection.dest_id) else {
                        continue;
                    };
                    let (new_source, source_is_internal) = point_id_map
                        .get(&connection.src_id)
                        .map(|source| (source.as_str(), true))
                        .unwrap_or((connection.src_id.as_str(), false));
                    if deleted_presentation.contains(new_source) {
                        continue;
                    }
                    let office_root_order = node
                        .parent_id
                        .as_deref()
                        .and_then(|parent_id| office_root_style_counts.get(parent_id))
                        .filter(|_| {
                            presentation_points
                                .get(&connection.dest_id)
                                .is_some_and(|point| {
                                    point.presentation_name.as_deref() == Some("node")
                                })
                        })
                        .map(|_| node.order.saturating_mul(2));
                    let source_order = if source_is_internal {
                        None
                    } else if office_root_order.is_some() {
                        office_root_order
                    } else {
                        connection
                            .src_order
                            .map(|order| order.saturating_add(order_delta).max(0))
                    };
                    let connection_id = guid_factory.next(&mut used_ids);
                    new_presentation_connections.push_str(&clone_connection_xml(
                        data_xml,
                        connection,
                        &connection_id,
                        new_source,
                        new_destination,
                        source_order,
                        None,
                    ));
                }
            }
        }

        if !cloned_mapping {
            let presentation_id = guid_factory.next(&mut used_ids);
            let connection_id = guid_factory.next(&mut used_ids);
            new_presentation_points
                .push_str(&minimal_presentation_point_xml(&presentation_id, &node.id));
            new_presentation_connections.push_str(&minimal_presentation_connection_xml(
                &connection_id,
                &node.id,
                &presentation_id,
            ));
        }
    }
    let new_point_xml = format!(
        "{logical_point_xml}{new_transition_points}{new_sibling_presentation_points}{new_presentation_points}"
    );
    append_to_list(
        data_xml,
        &parsed.point_list_range,
        parsed.point_list_insert,
        &new_point_xml,
        "dgm:ptLst",
        &mut patches,
    )?;

    let new_connections = format!(
        "{new_sibling_presentation_connections}{new_presentation_connections}{new_hierarchy_connections}"
    );
    for (index, connection) in parsed.connections.iter().enumerate() {
        if connection.hierarchy {
            if let Some(node) = desired.get(&connection.dest_id) {
                let primary = hierarchy_by_child
                    .get(&connection.dest_id)
                    .and_then(|items| items.first())
                    .copied();
                if primary != Some(index) || node.parent_id.is_none() {
                    patches.push(Patch {
                        range: connection.range.clone(),
                        replacement: String::new(),
                    });
                    continue;
                }
                let mut fragment = data_xml[connection.range.clone()].to_string();
                let parent = node.parent_id.as_deref().unwrap();
                fragment = set_start_tag_attribute(&fragment, "srcId", parent);
                fragment = set_start_tag_attribute(&fragment, "destId", &node.id);
                fragment = set_start_tag_attribute(&fragment, "srcOrd", &node.order.to_string());
                if connection.id.is_none() {
                    let id = guid_factory.next(&mut used_ids);
                    fragment = set_start_tag_attribute(&fragment, "modelId", &id);
                }
                if fragment != data_xml[connection.range.clone()] {
                    patches.push(Patch {
                        range: connection.range.clone(),
                        replacement: fragment,
                    });
                }
                continue;
            }
        }

        if deleted.contains(&connection.src_id)
            || deleted.contains(&connection.dest_id)
            || deleted_presentation.contains(&connection.src_id)
            || deleted_presentation.contains(&connection.dest_id)
        {
            patches.push(Patch {
                range: connection.range.clone(),
                replacement: String::new(),
            });
        }
    }

    if !new_connections.is_empty() {
        if let Some(range) = &parsed.connection_list_range {
            append_to_list(
                data_xml,
                range,
                parsed.connection_list_insert,
                &new_connections,
                "dgm:cxnLst",
                &mut patches,
            )?;
        } else {
            patches.push(Patch {
                range: parsed.point_list_range.end..parsed.point_list_range.end,
                replacement: format!(
                    "<dgm:cxnLst xmlns:dgm=\"{DGM_NS}\">{new_connections}</dgm:cxnLst>"
                ),
            });
        }
    }

    let edited = apply_patches(data_xml, patches)?;
    reorder_top_level_points(&edited, &desired)
}

fn reorder_top_level_points(
    xml: &str,
    desired: &HashMap<String, DesiredNode>,
) -> Result<String, String> {
    let top_level_ids: HashSet<&str> = desired
        .values()
        .filter(|node| node.parent_id.is_none())
        .map(|node| node.id.as_str())
        .collect();
    if top_level_ids.len() < 2 {
        return Ok(xml.to_string());
    }

    let parsed = parse_document(xml)?;
    let slots: Vec<&PointRecord> = parsed
        .points
        .iter()
        .filter(|point| point.editable && top_level_ids.contains(point.id.as_str()))
        .collect();
    if slots.len() != top_level_ids.len() {
        return Err("SmartArt top-level point set is incomplete after editing".to_string());
    }

    let mut ordered: Vec<&DesiredNode> = desired
        .values()
        .filter(|node| node.parent_id.is_none())
        .collect();
    ordered.sort_by(|a, b| {
        a.order
            .cmp(&b.order)
            .then(a.document_order.cmp(&b.document_order))
            .then(a.id.cmp(&b.id))
    });

    let fragments: HashMap<&str, &str> = slots
        .iter()
        .map(|point| (point.id.as_str(), &xml[point.range.clone()]))
        .collect();
    let mut patches = Vec::new();
    for (slot, node) in slots.iter().zip(ordered) {
        let replacement = *fragments
            .get(node.id.as_str())
            .ok_or_else(|| format!("SmartArt top-level point {} is missing", node.id))?;
        if &xml[slot.range.clone()] != replacement {
            patches.push(Patch {
                range: slot.range.clone(),
                replacement: replacement.to_string(),
            });
        }
    }
    apply_patches(xml, patches)
}

fn is_dgm(node: Node<'_, '_>, name: &str) -> bool {
    node.is_element()
        && node.tag_name().name() == name
        && matches!(node.tag_name().namespace(), Some(DGM_NS) | None)
}

fn is_a(node: Node<'_, '_>, name: &str) -> bool {
    node.is_element() && node.tag_name().name() == name && node.tag_name().namespace() == Some(A_NS)
}

fn parse_document(xml: &str) -> Result<ParsedSmartArt, String> {
    let document =
        roxmltree::Document::parse(xml).map_err(|error| format!("SmartArt XML: {error}"))?;
    let root = document.root_element();
    if !is_dgm(root, "dataModel") {
        return Err("SmartArt data part has no dgm:dataModel root".to_string());
    }
    let point_list = root
        .children()
        .find(|node| is_dgm(*node, "ptLst"))
        .ok_or("SmartArt data model has no dgm:ptLst")?;
    let connection_list = root.children().find(|node| is_dgm(*node, "cxnLst"));

    let mut points = Vec::new();
    for (document_order, point) in point_list
        .children()
        .filter(|node| is_dgm(*node, "pt"))
        .enumerate()
    {
        let Some(id) = point.attribute("modelId").filter(|id| !id.is_empty()) else {
            continue;
        };
        let kind = point.attribute("type").unwrap_or("node").to_string();
        let property_set = point.children().find(|node| is_dgm(*node, "prSet"));
        let text_container = point.children().find(|node| is_dgm(*node, "t"));
        let text = text_container.map(extract_rich_text).unwrap_or_default();
        let paragraphs = text_container
            .map(|container| smartart_text_paragraphs(xml, container))
            .unwrap_or_else(|| json!([]));
        let text_elements = text_container
            .into_iter()
            .flat_map(|node| node.descendants())
            .filter(|node| is_a(*node, "t"))
            .map(|node| TextElement {
                range: node.range(),
            })
            .collect();
        points.push(PointRecord {
            id: id.to_string(),
            kind: kind.clone(),
            text,
            paragraphs,
            range: point.range(),
            text_elements,
            text_container: text_container.map(|node| node.range()),
            presentation_assoc_id: property_set
                .and_then(|node| node.attribute("presAssocID"))
                .map(str::to_string),
            presentation_name: property_set
                .and_then(|node| node.attribute("presName"))
                .map(str::to_string),
            presentation_style_count: property_set
                .and_then(|node| node.attribute("presStyleCnt"))
                .and_then(|value| value.parse::<i64>().ok()),
            document_order,
            editable: is_logical_kind(&kind),
        });
    }

    let mut connections = Vec::new();
    if let Some(list) = connection_list {
        for (document_order, connection) in list
            .children()
            .filter(|node| is_dgm(*node, "cxn"))
            .enumerate()
        {
            let src_id = connection.attribute("srcId").unwrap_or("").to_string();
            let dest_id = connection.attribute("destId").unwrap_or("").to_string();
            let kind = connection.attribute("type").unwrap_or("parOf").to_string();
            connections.push(ConnectionRecord {
                id: connection.attribute("modelId").map(str::to_string),
                kind: kind.clone(),
                src_id,
                dest_id,
                hierarchy: kind == "parOf",
                src_order: connection
                    .attribute("srcOrd")
                    .and_then(|value| value.parse::<i64>().ok()),
                dest_order: connection
                    .attribute("destOrd")
                    .and_then(|value| value.parse::<i64>().ok()),
                sibling_transition_id: connection.attribute("sibTransId").map(str::to_string),
                range: connection.range(),
                document_order,
            });
        }
    }

    Ok(ParsedSmartArt {
        layout: infer_layout(root),
        points,
        connections,
        point_list_range: point_list.range(),
        point_list_insert: closing_tag_start(xml, point_list.range()),
        connection_list_range: connection_list.map(|node| node.range()),
        connection_list_insert: connection_list
            .and_then(|node| closing_tag_start(xml, node.range())),
    })
}

fn infer_layout(root: Node<'_, '_>) -> Option<String> {
    for name in ["layout", "layoutId", "loTypeId", "typeId"] {
        if let Some(value) = root.attribute(name).filter(|value| !value.is_empty()) {
            return Some(value.to_string());
        }
    }
    for node in root.descendants().filter(Node::is_element) {
        for attribute in node.attributes() {
            if matches!(
                attribute.name(),
                "layout" | "layoutId" | "loTypeId" | "typeId"
            ) && !attribute.value().is_empty()
            {
                return Some(attribute.value().to_string());
            }
        }
    }
    None
}

fn is_logical_kind(kind: &str) -> bool {
    !matches!(kind, "pres" | "parTrans" | "sibTrans")
}

fn exclusively_deleted_presentation_points(
    parsed: &ParsedSmartArt,
    deleted_logical: &HashSet<String>,
) -> HashSet<String> {
    let presentation_ids: HashSet<&str> = parsed
        .points
        .iter()
        .filter(|point| point.kind == "pres")
        .map(|point| point.id.as_str())
        .collect();
    let mut sources_by_destination = HashMap::<&str, Vec<&str>>::new();
    for connection in parsed
        .connections
        .iter()
        .filter(|connection| connection.kind == "presOf")
    {
        if presentation_ids.contains(connection.dest_id.as_str()) {
            sources_by_destination
                .entry(connection.dest_id.as_str())
                .or_default()
                .push(connection.src_id.as_str());
        }
    }

    let surviving_mappings: HashSet<&str> = sources_by_destination
        .iter()
        .filter(|(_, sources)| {
            sources
                .iter()
                .any(|source| !deleted_logical.contains(*source))
        })
        .map(|(destination, _)| *destination)
        .collect();
    let mut removed: HashSet<String> = sources_by_destination
        .iter()
        .filter(|(_, sources)| {
            !sources.is_empty()
                && sources
                    .iter()
                    .all(|source| deleted_logical.contains(*source))
        })
        .map(|(destination, _)| (*destination).to_string())
        .collect();

    // Presentation-only descendants may not have their own presOf mapping. Remove such a
    // descendant only when every presentation parent is already being removed. A shared or
    // independently mapped presentation point is deliberately retained.
    loop {
        let mut changed = false;
        for point_id in &presentation_ids {
            if removed.contains(*point_id) || surviving_mappings.contains(point_id) {
                continue;
            }
            let parents: Vec<&str> = parsed
                .connections
                .iter()
                .filter(|connection| {
                    connection.kind == "presParOf" && connection.dest_id == *point_id
                })
                .map(|connection| connection.src_id.as_str())
                .collect();
            if !parents.is_empty() && parents.iter().all(|parent| removed.contains(*parent)) {
                removed.insert((*point_id).to_string());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    removed
}

fn select_presentation_template(
    node: &DesiredNode,
    current: &HashMap<String, ModelNode>,
    desired: &HashMap<String, DesiredNode>,
    mapped_sources: &HashSet<String>,
) -> Option<String> {
    let mut candidates: Vec<&ModelNode> = current
        .values()
        .filter(|candidate| {
            desired.contains_key(&candidate.id) && mapped_sources.contains(&candidate.id)
        })
        .collect();
    candidates.sort_by(|left, right| {
        let left_same_parent = left.parent_id == node.parent_id;
        let right_same_parent = right.parent_id == node.parent_id;
        let left_same_kind = left.kind == node.kind;
        let right_same_kind = right.kind == node.kind;
        right_same_parent
            .cmp(&left_same_parent)
            .then(right_same_kind.cmp(&left_same_kind))
            .then(
                left.order
                    .abs_diff(node.order)
                    .cmp(&right.order.abs_diff(node.order)),
            )
            .then(left.document_order.cmp(&right.document_order))
            .then(left.id.cmp(&right.id))
    });
    candidates.first().map(|candidate| candidate.id.clone())
}

fn replace_exact_attribute_value(
    fragment: String,
    name: &str,
    old_value: &str,
    new_value: &str,
) -> String {
    let mut result = fragment;
    for quote in ['"', '\''] {
        let needle = format!(" {name}={quote}{}{quote}", escape_attr(old_value));
        let replacement = format!(" {name}={quote}{}{quote}", escape_attr(new_value));
        result = result.replace(&needle, &replacement);
    }
    result
}

fn clone_presentation_point_xml(
    xml: &str,
    point: &PointRecord,
    new_point_id: &str,
    template_logical_id: &str,
    new_logical_id: &str,
) -> String {
    let fragment = set_start_tag_attribute(&xml[point.range.clone()], "modelId", new_point_id);
    replace_exact_attribute_value(fragment, "presAssocID", template_logical_id, new_logical_id)
}

fn clone_connection_xml(
    xml: &str,
    connection: &ConnectionRecord,
    new_connection_id: &str,
    new_source_id: &str,
    new_destination_id: &str,
    source_order: Option<i64>,
    destination_order: Option<i64>,
) -> String {
    let mut fragment = xml[connection.range.clone()].to_string();
    fragment = set_start_tag_attribute(&fragment, "modelId", new_connection_id);
    fragment = set_start_tag_attribute(&fragment, "srcId", new_source_id);
    fragment = set_start_tag_attribute(&fragment, "destId", new_destination_id);
    if let Some(source_order) = source_order {
        fragment = set_start_tag_attribute(&fragment, "srcOrd", &source_order.to_string());
    }
    if let Some(destination_order) = destination_order {
        fragment = set_start_tag_attribute(&fragment, "destOrd", &destination_order.to_string());
    }
    fragment
}

fn presentation_style_point_xml(
    xml: &str,
    point: &PointRecord,
    new_point_id: &str,
    new_assoc_id: &str,
    style_index: i64,
    style_count: i64,
) -> String {
    let mut fragment = clone_presentation_point_xml(
        xml,
        point,
        new_point_id,
        point.presentation_assoc_id.as_deref().unwrap_or(""),
        new_assoc_id,
    );
    fragment = set_descendant_start_tag_attribute(&fragment, "prSet", "presAssocID", new_assoc_id);
    fragment = set_descendant_start_tag_attribute(
        &fragment,
        "prSet",
        "presStyleIdx",
        &style_index.to_string(),
    );
    set_descendant_start_tag_attribute(&fragment, "prSet", "presStyleCnt", &style_count.to_string())
}

fn sibling_presentation_point_xml(
    xml: &str,
    template: Option<&PointRecord>,
    new_point_id: &str,
    sibling_transition_id: &str,
) -> String {
    if let Some(template) = template {
        let mut fragment = clone_presentation_point_xml(
            xml,
            template,
            new_point_id,
            template.presentation_assoc_id.as_deref().unwrap_or(""),
            sibling_transition_id,
        );
        fragment = set_descendant_start_tag_attribute(
            &fragment,
            "prSet",
            "presAssocID",
            sibling_transition_id,
        );
        return fragment;
    }
    format!(
        "<dgm:pt xmlns:dgm=\"{DGM_NS}\" modelId=\"{}\" type=\"pres\"><dgm:prSet presAssocID=\"{}\" presName=\"sibTrans\" presStyleCnt=\"0\"/><dgm:spPr/></dgm:pt>",
        escape_attr(new_point_id),
        escape_attr(sibling_transition_id),
    )
}

fn minimal_presentation_parent_connection_xml(
    connection_id: &str,
    source_id: &str,
    destination_id: &str,
    source_order: i64,
) -> String {
    format!(
        "<dgm:cxn xmlns:dgm=\"{DGM_NS}\" modelId=\"{}\" type=\"presParOf\" srcId=\"{}\" destId=\"{}\" srcOrd=\"{}\" destOrd=\"0\"/>",
        escape_attr(connection_id),
        escape_attr(source_id),
        escape_attr(destination_id),
        source_order,
    )
}

fn transition_point_xml(point_id: &str, kind: &str, connection_id: &str) -> String {
    format!(
        "<dgm:pt xmlns:dgm=\"{DGM_NS}\" modelId=\"{}\" type=\"{}\" cxnId=\"{}\"><dgm:prSet/><dgm:spPr/></dgm:pt>",
        escape_attr(point_id),
        escape_attr(kind),
        escape_attr(connection_id),
    )
}

fn hierarchy_connection_xml(
    connection_id: &str,
    parent_id: &str,
    node_id: &str,
    source_order: i64,
    par_transition_id: &str,
    sibling_transition_id: &str,
) -> String {
    format!(
        "<dgm:cxn xmlns:dgm=\"{DGM_NS}\" modelId=\"{}\" srcId=\"{}\" destId=\"{}\" srcOrd=\"{}\" destOrd=\"0\" parTransId=\"{}\" sibTransId=\"{}\"/>",
        escape_attr(connection_id),
        escape_attr(parent_id),
        escape_attr(node_id),
        source_order,
        escape_attr(par_transition_id),
        escape_attr(sibling_transition_id),
    )
}

fn minimal_presentation_point_xml(point_id: &str, logical_id: &str) -> String {
    format!(
        "<dgm:pt xmlns:dgm=\"{DGM_NS}\" modelId=\"{}\" type=\"pres\"><dgm:prSet presAssocID=\"{}\"/></dgm:pt>",
        escape_attr(point_id),
        escape_attr(logical_id),
    )
}

fn minimal_presentation_connection_xml(
    connection_id: &str,
    logical_id: &str,
    presentation_id: &str,
) -> String {
    format!(
        "<dgm:cxn xmlns:dgm=\"{DGM_NS}\" modelId=\"{}\" type=\"presOf\" srcId=\"{}\" destId=\"{}\" srcOrd=\"0\" destOrd=\"0\"/>",
        escape_attr(connection_id),
        escape_attr(logical_id),
        escape_attr(presentation_id),
    )
}

fn element_inner_range(xml: &str, node: Node<'_, '_>) -> Result<Range<usize>, String> {
    let range = node.range();
    let raw = &xml[range.clone()];
    let open_end = start_tag_end(raw).ok_or("malformed XML element")?;
    let start = range.start + open_end + 1;
    if raw[..=open_end]
        .trim_end_matches('>')
        .trim_end()
        .ends_with('/')
    {
        return Ok(start..start);
    }
    let end = closing_tag_start(xml, range).ok_or("malformed XML closing tag")?;
    Ok(start..end)
}

fn smartart_text_wrapper(xml: &str, container: Node<'_, '_>) -> Result<String, String> {
    let inner = element_inner_range(xml, container)?;
    let mut namespaces = std::collections::BTreeMap::<String, String>::new();
    for namespace in container.namespaces() {
        namespaces.insert(
            namespace.name().unwrap_or("").to_string(),
            namespace.uri().to_string(),
        );
    }
    namespaces.insert(
        "xdr".to_string(),
        "http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing".to_string(),
    );
    namespaces.insert("a".to_string(), A_NS.to_string());
    let declarations = namespaces
        .into_iter()
        .map(|(prefix, uri)| {
            if prefix.is_empty() {
                format!(" xmlns=\"{}\"", escape_attr(&uri))
            } else {
                format!(" xmlns:{prefix}=\"{}\"", escape_attr(&uri))
            }
        })
        .collect::<String>();
    Ok(format!(
        "<xdr:sp{declarations}><xdr:spPr/><xdr:txBody>{}</xdr:txBody></xdr:sp>",
        &xml[inner]
    ))
}

fn wrapper_text_body_inner(wrapper: &str) -> Result<&str, String> {
    let document = roxmltree::Document::parse(wrapper)
        .map_err(|error| format!("SmartArt text wrapper XML: {error}"))?;
    let body = document
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "txBody")
        .ok_or("SmartArt text wrapper has no txBody")?;
    let range = element_inner_range(wrapper, body)?;
    Ok(&wrapper[range])
}

fn smartart_text_paragraphs(xml: &str, container: Node<'_, '_>) -> Value {
    let Ok(wrapper) = smartart_text_wrapper(xml, container) else {
        return json!([]);
    };
    let model = crate::native_shape_edit::parse_shape_model(&wrapper);
    model
        .get("paragraphs")
        .cloned()
        .unwrap_or_else(|| json!([]))
}

fn validate_smartart_paragraphs(paragraphs: &Value) -> Result<(), String> {
    let array = paragraphs
        .as_array()
        .ok_or("SmartArt node paragraphs must be an array")?;
    if array.is_empty() {
        return Err("SmartArt node paragraphs must contain at least one paragraph".to_string());
    }
    new_structured_rich_text_body(paragraphs).map(|_| ())
}

fn smartart_paragraphs_text(paragraphs: &Value) -> Result<String, String> {
    let paragraphs = paragraphs
        .as_array()
        .ok_or("SmartArt node paragraphs must be an array")?;
    let mut lines = Vec::with_capacity(paragraphs.len());
    for paragraph in paragraphs {
        let runs = paragraph
            .get("runs")
            .and_then(Value::as_array)
            .ok_or("SmartArt text paragraph runs must be an array")?;
        let mut line = String::new();
        for run in runs {
            line.push_str(
                run.get("text")
                    .and_then(Value::as_str)
                    .ok_or("SmartArt text run text must be a string")?,
            );
        }
        lines.push(line);
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    Ok(lines.join("\n"))
}

fn text_without_source_indices(paragraphs: &Value) -> Value {
    let mut value = paragraphs.clone();
    if let Some(paragraphs) = value.as_array_mut() {
        for paragraph in paragraphs {
            if let Some(paragraph) = paragraph.as_object_mut() {
                paragraph.remove("sourceIndex");
                if let Some(runs) = paragraph.get_mut("runs").and_then(Value::as_array_mut) {
                    for run in runs {
                        if let Some(run) = run.as_object_mut() {
                            run.remove("sourceIndex");
                        }
                    }
                }
            }
        }
    }
    value
}

fn new_structured_rich_text_body(paragraphs: &Value) -> Result<String, String> {
    let wrapper = format!(
        "<xdr:sp xmlns:xdr=\"http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing\" xmlns:a=\"{A_NS}\"><xdr:spPr/><xdr:txBody><a:bodyPr/><a:lstStyle/></xdr:txBody></xdr:sp>"
    );
    // Source identities are meaningful only while patching the original text
    // body.  Validation/new-node synthesis uses an empty wrapper, so carrying
    // those indices into it would incorrectly look like an out-of-range edit.
    let paragraphs = text_without_source_indices(paragraphs);
    let edited =
        crate::native_shape_edit::apply_shape_edit(&wrapper, &json!({"paragraphs":paragraphs}))
            .map_err(|error| format!("SmartArt structured text: {error}"))?;
    Ok(wrapper_text_body_inner(&edited)?.to_string())
}

fn patch_smartart_point_text(
    xml: &str,
    point: &PointRecord,
    paragraphs: &Value,
) -> Result<String, String> {
    validate_smartart_paragraphs(paragraphs)?;
    let document =
        roxmltree::Document::parse(xml).map_err(|error| format!("SmartArt XML: {error}"))?;
    let point_node = document
        .descendants()
        .find(|node| is_dgm(*node, "pt") && node.range().start == point.range.start)
        .ok_or("SmartArt point disappeared while editing text")?;
    let mut fragment = xml[point.range.clone()].to_string();
    if let Some(container) = point_node.children().find(|node| is_dgm(*node, "t")) {
        let wrapper = smartart_text_wrapper(xml, container)?;
        let edited =
            crate::native_shape_edit::apply_shape_edit(&wrapper, &json!({"paragraphs":paragraphs}))
                .map_err(|error| format!("SmartArt structured text: {error}"))?;
        let inner = wrapper_text_body_inner(&edited)?;
        let container_range = container.range();
        let mut container_xml = xml[container_range.clone()].to_string();
        let inner_range = element_inner_range(xml, container)?;
        container_xml.replace_range(
            inner_range.start - container_range.start..inner_range.end - container_range.start,
            inner,
        );
        fragment.replace_range(
            container_range.start - point.range.start..container_range.end - point.range.start,
            &container_xml,
        );
    } else {
        let close = fragment.rfind("</").ok_or("malformed SmartArt point")?;
        fragment.insert_str(
            close,
            &format!(
                "<dgm:t xmlns:dgm=\"{DGM_NS}\" xmlns:a=\"{A_NS}\">{}</dgm:t>",
                new_structured_rich_text_body(paragraphs)?
            ),
        );
    }
    Ok(fragment)
}

fn extract_rich_text(container: Node<'_, '_>) -> String {
    let mut paragraphs: Vec<String> = container
        .descendants()
        .filter(|node| is_a(*node, "p"))
        .map(|paragraph| {
            paragraph
                .descendants()
                .filter(|node| is_a(*node, "t"))
                .filter_map(|node| node.text())
                .collect::<String>()
        })
        .collect();
    // Office commonly leaves a terminal empty paragraph as a formatting
    // carrier. It is not user-visible SmartArt text.
    while paragraphs.last().is_some_and(String::is_empty) {
        paragraphs.pop();
    }
    if paragraphs.is_empty() {
        container
            .descendants()
            .filter(|node| is_a(*node, "t"))
            .filter_map(|node| node.text())
            .collect()
    } else {
        paragraphs.join("\n")
    }
}

fn build_model_nodes(parsed: &ParsedSmartArt) -> Vec<ModelNode> {
    let point_ids: HashSet<&str> = parsed
        .points
        .iter()
        .map(|point| point.id.as_str())
        .collect();
    let mut hierarchy: HashMap<&str, (&str, i64, usize)> = HashMap::new();
    let mut fallback_by_parent = HashMap::<&str, i64>::new();
    for connection in parsed
        .connections
        .iter()
        .filter(|connection| connection.hierarchy)
    {
        if !point_ids.contains(connection.src_id.as_str())
            || !point_ids.contains(connection.dest_id.as_str())
        {
            continue;
        }
        let fallback = fallback_by_parent
            .entry(connection.src_id.as_str())
            .or_default();
        let order = connection.src_order.unwrap_or(*fallback);
        *fallback = (*fallback).max(order + 1);
        hierarchy.entry(connection.dest_id.as_str()).or_insert((
            connection.src_id.as_str(),
            order,
            connection.document_order,
        ));
    }

    let mut root_order = 0i64;
    parsed
        .points
        .iter()
        .filter(|point| point.editable)
        .map(|point| {
            let (parent_id, order) = hierarchy
                .get(point.id.as_str())
                .map(|(parent, order, _)| (Some((*parent).to_string()), *order))
                .unwrap_or_else(|| {
                    let order = root_order;
                    root_order += 1;
                    (None, order)
                });
            ModelNode {
                id: point.id.clone(),
                parent_id,
                text: point.text.clone(),
                paragraphs: point.paragraphs.clone(),
                order,
                kind: point.kind.clone(),
                document_order: point.document_order,
            }
        })
        .collect()
}

fn parse_node_array(value: &Value, field: &str) -> Result<Vec<NodeEdit>, String> {
    let array = value
        .as_array()
        .ok_or_else(|| format!("SmartArt {field} must be an array"))?;
    array.iter().map(parse_node_edit).collect()
}

fn collect_node_arrays(
    object: &serde_json::Map<String, Value>,
    names: &[&str],
) -> Result<Vec<NodeEdit>, String> {
    let mut result = Vec::new();
    for name in names {
        let Some(value) = object.get(*name) else {
            continue;
        };
        if let Some(array) = value.as_array() {
            for item in array {
                result.push(parse_node_edit(item)?);
            }
        } else if value.is_object() {
            result.push(parse_node_edit(value)?);
        } else {
            return Err(format!("SmartArt {name} must be an object or array"));
        }
    }
    Ok(result)
}

fn parse_node_edit(value: &Value) -> Result<NodeEdit, String> {
    let object = value
        .as_object()
        .ok_or("SmartArt node edit must be an object")?;
    let key = object
        .get("id")
        .or_else(|| object.get("tempId"))
        .or_else(|| object.get("key"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let text = match object.get("text") {
        Some(Value::String(value)) => Some(value.clone()),
        Some(_) => return Err("SmartArt node text must be a string".to_string()),
        None => None,
    };
    let paragraphs = match object.get("paragraphs") {
        Some(Value::Array(value)) => Some(Value::Array(value.clone())),
        Some(_) => return Err("SmartArt node paragraphs must be an array".to_string()),
        None => None,
    };
    let parent_value = object.get("parentId").or_else(|| object.get("parent_id"));
    let parent = match parent_value {
        None => ParentEdit::Keep,
        Some(Value::Null) => ParentEdit::Set(None),
        Some(Value::String(value)) if value.is_empty() => ParentEdit::Set(None),
        Some(Value::String(value)) => ParentEdit::Set(Some(value.clone())),
        Some(_) => return Err("SmartArt parentId must be a string or null".to_string()),
    };
    let order = match object.get("order") {
        Some(value) => Some(
            value
                .as_i64()
                .ok_or("SmartArt node order must be an integer")?,
        ),
        None => None,
    };
    let kind = match object.get("kind") {
        Some(Value::String(value)) if !value.is_empty() => Some(value.clone()),
        Some(Value::String(_)) => Some("node".to_string()),
        Some(_) => return Err("SmartArt node kind must be a string".to_string()),
        None => None,
    };
    Ok(NodeEdit {
        key,
        text,
        paragraphs,
        parent,
        order,
        kind,
    })
}

fn assign_new_ids(
    specs: &[NodeEdit],
    current: &HashMap<String, ModelNode>,
    force_new: bool,
    temp_ids: &mut HashMap<String, String>,
    used_ids: &mut HashSet<String>,
    factory: &mut GuidFactory,
) -> Result<(), String> {
    for (index, spec) in specs.iter().enumerate() {
        let existing = spec
            .key
            .as_ref()
            .is_some_and(|key| current.contains_key(key));
        if existing && !force_new {
            continue;
        }
        if existing && force_new {
            return Err(format!(
                "SmartArt node {} already exists",
                spec.key.as_deref().unwrap()
            ));
        }
        let temporary = spec.key.clone().unwrap_or_else(|| format!("__new_{index}"));
        if temp_ids.contains_key(&temporary) {
            return Err(format!("duplicate SmartArt temporary id {temporary}"));
        }
        let generated = factory.next(used_ids);
        temp_ids.insert(temporary, generated);
    }
    Ok(())
}

fn upsert_spec(
    spec: &NodeEdit,
    index: usize,
    current: &HashMap<String, ModelNode>,
    temp_ids: &HashMap<String, String>,
    desired: &mut HashMap<String, DesiredNode>,
    force_new: bool,
) -> Result<(), String> {
    let temporary = spec.key.clone().unwrap_or_else(|| format!("__new_{index}"));
    let is_existing = !force_new && current.contains_key(&temporary);
    let id = if is_existing {
        temporary.clone()
    } else {
        temp_ids
            .get(&temporary)
            .cloned()
            .ok_or_else(|| format!("unknown SmartArt node {temporary}"))?
    };

    let mut node = if is_existing {
        let original = current.get(&id).unwrap().clone();
        DesiredNode {
            id: id.clone(),
            parent_id: original.parent_id.clone(),
            text: original.text.clone(),
            paragraphs: original.paragraphs.clone(),
            order: original.order,
            kind: original.kind.clone(),
            document_order: original.document_order,
            original: Some(original),
        }
    } else if let Some(existing) = desired.get(&id) {
        existing.clone()
    } else {
        DesiredNode {
            id: id.clone(),
            parent_id: None,
            text: String::new(),
            paragraphs: json!([]),
            order: i64::MAX / 4,
            kind: "node".to_string(),
            document_order: usize::MAX - index,
            original: None,
        }
    };

    let paragraphs_changed = spec
        .paragraphs
        .as_ref()
        .is_some_and(|paragraphs| paragraphs != &node.paragraphs);
    if let Some(text) = &spec.text {
        node.text = text.clone();
    }
    if let Some(paragraphs) = &spec.paragraphs {
        validate_smartart_paragraphs(paragraphs)?;
        if paragraphs_changed || spec.text.is_none() {
            node.paragraphs = paragraphs.clone();
            node.text = smartart_paragraphs_text(paragraphs)?;
        }
    }
    if let Some(order) = spec.order {
        node.order = order;
    }
    if let Some(kind) = &spec.kind {
        node.kind = kind.clone();
    }
    if let ParentEdit::Set(parent) = &spec.parent {
        node.parent_id = parent.clone();
    }
    desired.insert(id, node);
    Ok(())
}

fn collect_delete_ids(object: &serde_json::Map<String, Value>) -> Result<Vec<String>, String> {
    let mut result = Vec::new();
    for name in ["deleteIds", "deleteNodes", "delete"] {
        let Some(value) = object.get(name) else {
            continue;
        };
        let values: Vec<&Value> = if let Some(array) = value.as_array() {
            array.iter().collect()
        } else {
            vec![value]
        };
        for item in values {
            if let Some(id) = item.as_str() {
                result.push(id.to_string());
            } else if let Some(id) = item.get("id").and_then(Value::as_str) {
                result.push(id.to_string());
            } else {
                return Err(format!(
                    "SmartArt {name} entries must be ids or objects with id"
                ));
            }
        }
    }
    Ok(result)
}

fn apply_deletions(
    ids: &[String],
    cascade: bool,
    current: &HashMap<String, ModelNode>,
    desired: &mut HashMap<String, DesiredNode>,
) -> Result<(), String> {
    let mut deleted = HashSet::new();
    for id in ids {
        if !desired.contains_key(id) {
            return Err(format!("unknown SmartArt node {id}"));
        }
        deleted.insert(id.clone());
    }
    if cascade {
        loop {
            let descendants: Vec<String> = desired
                .values()
                .filter(|node| {
                    node.parent_id
                        .as_ref()
                        .is_some_and(|parent| deleted.contains(parent))
                })
                .map(|node| node.id.clone())
                .collect();
            let before = deleted.len();
            deleted.extend(descendants);
            if deleted.len() == before {
                break;
            }
        }
    } else {
        for id in &deleted {
            let replacement_parent = current.get(id).and_then(|node| node.parent_id.clone());
            for node in desired.values_mut() {
                if node.parent_id.as_ref() == Some(id) {
                    node.parent_id = replacement_parent.clone();
                }
            }
        }
    }
    for id in deleted {
        desired.remove(&id);
    }
    Ok(())
}

fn resolve_parent_temp_ids(
    temp_ids: &HashMap<String, String>,
    desired: &mut HashMap<String, DesiredNode>,
) {
    for node in desired.values_mut() {
        if let Some(parent) = node.parent_id.clone() {
            if let Some(generated) = temp_ids.get(&parent) {
                node.parent_id = Some(generated.clone());
            }
        }
    }
}

fn validate_tree(
    parsed: &ParsedSmartArt,
    desired: &HashMap<String, DesiredNode>,
) -> Result<(), String> {
    let opaque_ids: HashSet<&str> = parsed
        .points
        .iter()
        .filter(|point| !point.editable)
        .map(|point| point.id.as_str())
        .collect();
    for node in desired.values() {
        if let Some(parent) = &node.parent_id {
            if parent == &node.id {
                return Err(format!("SmartArt node {} cannot parent itself", node.id));
            }
            if !desired.contains_key(parent) && !opaque_ids.contains(parent.as_str()) {
                return Err(format!("SmartArt parent {parent} does not exist"));
            }
        }
    }
    for node in desired.values() {
        let mut seen = HashSet::new();
        let mut cursor = Some(node.id.as_str());
        while let Some(id) = cursor {
            if !seen.insert(id) {
                return Err(format!("SmartArt hierarchy contains a cycle at {id}"));
            }
            cursor = desired.get(id).and_then(|item| item.parent_id.as_deref());
        }
    }
    Ok(())
}

fn normalize_changed_sibling_orders(
    current: &HashMap<String, ModelNode>,
    desired: &mut HashMap<String, DesiredNode>,
) {
    let mut changed_parents = HashSet::<Option<String>>::new();
    for old in current.values() {
        match desired.get(&old.id) {
            None => {
                changed_parents.insert(old.parent_id.clone());
            }
            Some(new) => {
                if old.parent_id != new.parent_id || old.order != new.order {
                    changed_parents.insert(old.parent_id.clone());
                    changed_parents.insert(new.parent_id.clone());
                }
            }
        }
    }
    for node in desired.values().filter(|node| node.original.is_none()) {
        changed_parents.insert(node.parent_id.clone());
    }

    for parent in changed_parents {
        let mut children: Vec<(String, i64, usize)> = desired
            .values()
            .filter(|node| node.parent_id == parent)
            .map(|node| (node.id.clone(), node.order, node.document_order))
            .collect();
        children.sort_by(|a, b| a.1.cmp(&b.1).then(a.2.cmp(&b.2)).then(a.0.cmp(&b.0)));
        for (order, (id, _, _)) in children.into_iter().enumerate() {
            if let Some(node) = desired.get_mut(&id) {
                node.order = order as i64;
            }
        }
    }
}

fn edit_existing_point(
    xml: &str,
    point: &PointRecord,
    node: &DesiredNode,
    text_changed: bool,
    paragraphs_changed: bool,
    kind_changed: bool,
) -> Result<String, String> {
    let mut fragment = xml[point.range.clone()].to_string();
    if paragraphs_changed {
        let replacement = patch_smartart_point_text(xml, point, &node.paragraphs)?;
        fragment = replacement;
    } else if text_changed {
        let mut local_patches = Vec::new();
        for (index, target) in point.text_elements.iter().enumerate() {
            let relative =
                (target.range.start - point.range.start)..(target.range.end - point.range.start);
            let value = if index == 0 { node.text.as_str() } else { "" };
            local_patches.push(Patch {
                range: relative.clone(),
                replacement: replace_text_element(&fragment[relative], value)?,
            });
        }
        if !local_patches.is_empty() {
            fragment = apply_patches(&fragment, local_patches)?;
        } else {
            let rich = new_rich_text_body(&node.text);
            if let Some(container_range) = &point.text_container {
                let relative = (container_range.start - point.range.start)
                    ..(container_range.end - point.range.start);
                let raw = &fragment[relative.clone()];
                let replacement = if let Some(close) = raw.rfind("</") {
                    let mut result = raw.to_string();
                    result.insert_str(close, &rich);
                    result
                } else {
                    expand_empty_element(raw, &rich)?
                };
                fragment = apply_patches(
                    &fragment,
                    vec![Patch {
                        range: relative,
                        replacement,
                    }],
                )?;
            } else {
                let close = fragment.rfind("</").ok_or("malformed SmartArt point")?;
                fragment.insert_str(
                    close,
                    &format!("<dgm:t xmlns:dgm=\"{DGM_NS}\" xmlns:a=\"{A_NS}\">{rich}</dgm:t>"),
                );
            }
        }
    }
    // Start-tag edits run last because inserting an attribute shifts every
    // descendant byte range captured from the source document.
    if kind_changed {
        fragment = if node.kind == "node" {
            remove_start_tag_attribute(&fragment, "type")
        } else {
            set_start_tag_attribute(&fragment, "type", &node.kind)
        };
    }
    Ok(fragment)
}

fn new_point_xml(node: &DesiredNode) -> String {
    let rich_text = if node
        .paragraphs
        .as_array()
        .is_some_and(|items| !items.is_empty())
    {
        new_structured_rich_text_body(&node.paragraphs)
            .unwrap_or_else(|_| new_rich_text_body(&node.text))
    } else {
        new_rich_text_body(&node.text)
    };
    let type_attribute = if node.kind == "node" {
        String::new()
    } else {
        format!(" type=\"{}\"", escape_attr(&node.kind))
    };
    format!(
        "<dgm:pt xmlns:dgm=\"{DGM_NS}\" xmlns:a=\"{A_NS}\" modelId=\"{}\"{type_attribute}><dgm:prSet/><dgm:spPr/><dgm:t>{}</dgm:t></dgm:pt>",
        escape_attr(&node.id),
        rich_text,
    )
}

fn new_rich_text_body(text: &str) -> String {
    let paragraphs: Vec<&str> = text.split('\n').collect();
    let paragraphs = paragraphs
        .iter()
        .map(|line| {
            format!(
                "<a:p><a:r><a:t xml:space=\"preserve\">{}</a:t></a:r></a:p>",
                escape_text(line)
            )
        })
        .collect::<String>();
    format!("<a:bodyPr/><a:lstStyle/>{paragraphs}")
}

fn replace_text_element(raw: &str, value: &str) -> Result<String, String> {
    let end = start_tag_end(raw).ok_or("malformed DrawingML text element")?;
    let qname = start_tag_name(raw).ok_or("malformed DrawingML text element name")?;
    let mut opening = raw[..=end].to_string();
    if value.starts_with(char::is_whitespace)
        || value.ends_with(char::is_whitespace)
        || value.contains('\n')
    {
        opening = set_start_tag_attribute(&opening, "xml:space", "preserve");
    }
    let closing = raw.rfind("</").map(|index| raw[index..].to_string());
    if raw[..end].trim_end().ends_with('/') {
        opening = opening
            .trim_end_matches('>')
            .trim_end()
            .trim_end_matches('/')
            .to_string()
            + ">";
        Ok(format!("{opening}{}</{qname}>", escape_text(value)))
    } else {
        Ok(format!(
            "{opening}{}{}",
            escape_text(value),
            closing.unwrap_or_else(|| format!("</{qname}>"))
        ))
    }
}

fn append_to_list(
    xml: &str,
    range: &Range<usize>,
    insert_at: Option<usize>,
    additions: &str,
    qualified_name: &str,
    patches: &mut Vec<Patch>,
) -> Result<(), String> {
    if additions.is_empty() {
        return Ok(());
    }
    if let Some(position) = insert_at {
        patches.push(Patch {
            range: position..position,
            replacement: additions.to_string(),
        });
    } else {
        let replacement = expand_empty_element(&xml[range.clone()], additions).or_else(|_| {
            Ok::<String, String>(format!(
                "<{qualified_name} xmlns:dgm=\"{DGM_NS}\">{additions}</{qualified_name}>"
            ))
        })?;
        patches.push(Patch {
            range: range.clone(),
            replacement,
        });
    }
    Ok(())
}

fn expand_empty_element(raw: &str, content: &str) -> Result<String, String> {
    let end = start_tag_end(raw).ok_or("malformed empty XML element")?;
    if !raw[..end].trim_end().ends_with('/') {
        return Err("XML element is not empty".to_string());
    }
    let qname = start_tag_name(raw).ok_or("malformed empty XML element name")?;
    let opening = raw[..end]
        .trim_end_matches('>')
        .trim_end()
        .trim_end_matches('/')
        .to_string();
    Ok(format!("{opening}>{content}</{qname}>"))
}

fn closing_tag_start(xml: &str, range: Range<usize>) -> Option<usize> {
    xml[range.clone()]
        .rfind("</")
        .map(|offset| range.start + offset)
}

fn start_tag_end(xml: &str) -> Option<usize> {
    let mut quote = None;
    for (index, character) in xml.char_indices() {
        match (quote, character) {
            (Some(active), current) if active == current => quote = None,
            (None, '\'' | '"') => quote = Some(character),
            (None, '>') => return Some(index),
            _ => {}
        }
    }
    None
}

fn start_tag_name(xml: &str) -> Option<&str> {
    let start = xml.find('<')? + 1;
    let end = xml[start..].find(|character: char| {
        character.is_whitespace() || character == '/' || character == '>'
    })? + start;
    Some(&xml[start..end])
}

fn set_start_tag_attribute(fragment: &str, name: &str, value: &str) -> String {
    let Some(tag_end) = start_tag_end(fragment) else {
        return fragment.to_string();
    };
    let bytes = fragment.as_bytes();
    let mut cursor = fragment.find('<').unwrap_or(0) + 1;
    while cursor < tag_end && !bytes[cursor].is_ascii_whitespace() && bytes[cursor] != b'/' {
        cursor += 1;
    }
    while cursor < tag_end {
        while cursor < tag_end && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= tag_end || bytes[cursor] == b'/' {
            break;
        }
        let attr_start = cursor;
        while cursor < tag_end && !bytes[cursor].is_ascii_whitespace() && bytes[cursor] != b'=' {
            cursor += 1;
        }
        let attr_name = &fragment[attr_start..cursor];
        while cursor < tag_end && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= tag_end || bytes[cursor] != b'=' {
            break;
        }
        cursor += 1;
        while cursor < tag_end && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= tag_end || !matches!(bytes[cursor], b'\'' | b'"') {
            break;
        }
        let quote = bytes[cursor];
        let value_start = cursor + 1;
        cursor = value_start;
        while cursor < tag_end && bytes[cursor] != quote {
            cursor += 1;
        }
        if attr_name == name {
            let mut result = fragment.to_string();
            result.replace_range(value_start..cursor, &escape_attr(value));
            return result;
        }
        cursor += 1;
    }
    let mut result = fragment.to_string();
    let insert = if fragment[..tag_end].trim_end().ends_with('/') {
        fragment[..tag_end].rfind('/').unwrap_or(tag_end)
    } else {
        tag_end
    };
    result.insert_str(insert, &format!(" {name}=\"{}\"", escape_attr(value)));
    result
}

fn remove_start_tag_attribute(fragment: &str, name: &str) -> String {
    let Some(tag_end) = start_tag_end(fragment) else {
        return fragment.to_string();
    };
    let bytes = fragment.as_bytes();
    let mut cursor = fragment.find('<').unwrap_or(0) + 1;
    while cursor < tag_end && !bytes[cursor].is_ascii_whitespace() && bytes[cursor] != b'/' {
        cursor += 1;
    }
    while cursor < tag_end {
        let whitespace_start = cursor;
        while cursor < tag_end && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= tag_end || bytes[cursor] == b'/' {
            break;
        }
        let attribute_start = cursor;
        while cursor < tag_end && !bytes[cursor].is_ascii_whitespace() && bytes[cursor] != b'=' {
            cursor += 1;
        }
        let attribute_name = &fragment[attribute_start..cursor];
        while cursor < tag_end && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= tag_end || bytes[cursor] != b'=' {
            break;
        }
        cursor += 1;
        while cursor < tag_end && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= tag_end || !matches!(bytes[cursor], b'\'' | b'"') {
            break;
        }
        let quote = bytes[cursor];
        cursor += 1;
        while cursor < tag_end && bytes[cursor] != quote {
            cursor += 1;
        }
        if cursor >= tag_end {
            break;
        }
        cursor += 1;
        if attribute_name == name {
            let mut result = fragment.to_string();
            let remove_start = if whitespace_start < attribute_start {
                whitespace_start
            } else {
                attribute_start
            };
            result.replace_range(remove_start..cursor, "");
            return result;
        }
    }
    fragment.to_string()
}

fn set_descendant_start_tag_attribute(
    fragment: &str,
    local_name: &str,
    attribute_name: &str,
    value: &str,
) -> String {
    let mut cursor = 0usize;
    while let Some(relative_start) = fragment[cursor..].find('<') {
        let start = cursor + relative_start;
        let name_start = start + 1;
        if name_start >= fragment.len()
            || matches!(fragment.as_bytes()[name_start], b'/' | b'!' | b'?')
        {
            cursor = name_start.saturating_add(1);
            continue;
        }
        let name_end = fragment[name_start..]
            .find(|character: char| {
                character.is_whitespace() || character == '/' || character == '>'
            })
            .map(|offset| name_start + offset)
            .unwrap_or(fragment.len());
        let qualified_name = &fragment[name_start..name_end];
        if qualified_name.rsplit(':').next() == Some(local_name) {
            let replacement = set_start_tag_attribute(&fragment[start..], attribute_name, value);
            let mut result = fragment[..start].to_string();
            result.push_str(&replacement);
            return result;
        }
        cursor = name_end.max(name_start + 1);
    }
    fragment.to_string()
}

fn apply_patches(xml: &str, mut patches: Vec<Patch>) -> Result<String, String> {
    patches.retain(|patch| patch.range.start <= patch.range.end && patch.range.end <= xml.len());
    patches.sort_by(|a, b| {
        b.range
            .start
            .cmp(&a.range.start)
            .then(b.range.end.cmp(&a.range.end))
    });
    let mut previous_start = xml.len();
    for patch in &patches {
        if patch.range.end > previous_start {
            return Err("overlapping SmartArt XML edits".to_string());
        }
        previous_start = patch.range.start;
    }
    let mut result = xml.to_string();
    for patch in patches {
        result.replace_range(patch.range, &patch.replacement);
    }
    Ok(result)
}

fn collect_model_ids(xml: &str) -> Result<HashSet<String>, String> {
    let document =
        roxmltree::Document::parse(xml).map_err(|error| format!("SmartArt XML: {error}"))?;
    Ok(document
        .descendants()
        .filter(Node::is_element)
        .filter_map(|node| node.attribute("modelId"))
        .map(|value| value.to_ascii_uppercase())
        .collect())
}

struct GuidFactory {
    seed: Vec<u8>,
    counter: u64,
}

impl GuidFactory {
    fn new(xml: &str, edit: &Value, ids: &HashSet<String>) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(xml.as_bytes());
        hasher.update(edit.to_string().as_bytes());
        let mut sorted: Vec<&String> = ids.iter().collect();
        sorted.sort();
        for id in sorted {
            hasher.update(id.as_bytes());
        }
        Self {
            seed: hasher.finalize().to_vec(),
            counter: 0,
        }
    }

    fn next(&mut self, used: &mut HashSet<String>) -> String {
        loop {
            let mut hasher = Sha256::new();
            hasher.update(&self.seed);
            hasher.update(self.counter.to_le_bytes());
            self.counter += 1;
            let digest = hasher.finalize();
            let mut bytes = [0u8; 16];
            bytes.copy_from_slice(&digest[..16]);
            bytes[6] = (bytes[6] & 0x0f) | 0x40;
            bytes[8] = (bytes[8] & 0x3f) | 0x80;
            let id = format!(
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
            );
            if used.insert(id.clone()) {
                return id;
            }
        }
    }
}

fn escape_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_attr(value: &str) -> String {
    escape_text(value)
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<dgm:dataModel xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:x="urn:keep" layoutId="urn:test:layout">
  <dgm:ptLst keep="pt-list">
    <dgm:pt modelId="ROOT" type="doc" x:unknown="keep-root"><dgm:prSet/><dgm:t><a:p><a:r><a:t>Root</a:t></a:r></a:p></dgm:t></dgm:pt>
    <dgm:pt modelId="A" type="node" x:unknown="keep-a"><dgm:prSet custom="stay"/><dgm:t x:keep="text-container"><a:p x:keep="paragraph"><a:pPr algn="ctr"><x:paragraphOpaque marker="keep"/></a:pPr><a:r x:keep="run"><a:rPr b="1" i="0" u="sng" sz="1200" lang="en-US" x:keep="properties"><a:solidFill><a:schemeClr val="accent2"><a:tint val="20000"/><a:alpha val="70000"/><x:colorTransform marker="keep"/></a:schemeClr></a:solidFill><a:latin typeface="Aptos" pitchFamily="34"/><x:runPropertiesOpaque marker="keep"/></a:rPr><a:t>Alpha</a:t><x:runOpaque marker="keep"/></a:r><a:endParaRPr lang="en-US"/></a:p></dgm:t><dgm:extLst><x:opaque value="keep"/></dgm:extLst></dgm:pt>
    <dgm:pt modelId="B" type="node"><dgm:t><a:p><a:r><a:t>Beta</a:t></a:r></a:p></dgm:t></dgm:pt>
    <dgm:pt modelId="PRES" type="pres" x:opaque="presentation"><dgm:prSet presAssocID="A" x:template="alpha"/></dgm:pt>
    <dgm:pt modelId="PRES_B" type="pres" x:opaque="presentation-b"><dgm:prSet presAssocID="B" x:template="beta"/></dgm:pt>
    <dgm:pt modelId="PRES_KEEP" type="pres" x:opaque="unrelated"><dgm:prSet x:sentinel="keep"/></dgm:pt>
  </dgm:ptLst>
  <dgm:cxnLst keep="cxn-list">
    <dgm:cxn modelId="C1" type="parOf" srcId="ROOT" destId="A" srcOrd="0" destOrd="0" x:unknown="keep-cxn"><dgm:extLst><x:opaque/></dgm:extLst></dgm:cxn>
    <dgm:cxn modelId="C2" type="parOf" srcId="ROOT" destId="B" srcOrd="1" destOrd="0"/>
    <dgm:cxn modelId="CP" type="presOf" srcId="A" destId="PRES" srcOrd="0" destOrd="0" x:unknown="keep-pres"/>
    <dgm:cxn modelId="CPB" type="presOf" srcId="B" destId="PRES_B" srcOrd="0" destOrd="0" x:unknown="keep-pres-b"/>
  </dgm:cxnLst>
  <dgm:extLst><x:wholeDocumentOpaque/></dgm:extLst>
</dgm:dataModel>"#;

    const RICH_SEQUENCE_XML: &str = r#"<dgm:dataModel xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:x="urn:keep"><dgm:ptLst><dgm:pt modelId="ROOT" type="doc"><dgm:prSet/></dgm:pt><dgm:pt modelId="A"><dgm:prSet/><dgm:t x:keep="body"><a:bodyPr/><a:lstStyle/><a:p x:keep="paragraph-drop"><a:r x:keep="run-drop"><a:t>Drop</a:t><x:runOpaque marker="drop"/></a:r><a:r x:keep="run-keep"><a:rPr b="1"><x:styleOpaque marker="keep"/></a:rPr><a:t>Keep</a:t><x:runOpaque marker="keep"/></a:r></a:p><a:p x:keep="paragraph-keep"><a:r x:keep="tail-keep"><a:t>Tail</a:t><x:tailOpaque marker="keep"/></a:r></a:p></dgm:t></dgm:pt></dgm:ptLst><dgm:cxnLst><dgm:cxn modelId="C1" type="parOf" srcId="ROOT" destId="A" srcOrd="0" destOrd="0"/></dgm:cxnLst></dgm:dataModel>"#;

    const OFFICE_ROOT_XML: &str = r#"<dgm:dataModel xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <dgm:ptLst>
    <dgm:pt modelId="ROOT" type="doc"><dgm:prSet/></dgm:pt>
    <dgm:pt modelId="N1"><dgm:prSet/><dgm:t><a:p><a:r><a:t>One</a:t></a:r></a:p></dgm:t></dgm:pt>
    <dgm:pt modelId="P1" type="parTrans" cxnId="H1"><dgm:prSet/><dgm:spPr/></dgm:pt>
    <dgm:pt modelId="S1" type="sibTrans" cxnId="H1"><dgm:prSet/><dgm:spPr/></dgm:pt>
    <dgm:pt modelId="N2"><dgm:prSet/><dgm:t><a:p><a:r><a:t>Two</a:t></a:r></a:p></dgm:t></dgm:pt>
    <dgm:pt modelId="P2" type="parTrans" cxnId="H2"><dgm:prSet/><dgm:spPr/></dgm:pt>
    <dgm:pt modelId="S2" type="sibTrans" cxnId="H2"><dgm:prSet/><dgm:spPr/></dgm:pt>
    <dgm:pt modelId="P_ROOT" type="pres"><dgm:prSet presAssocID="ROOT" presName="diagram" presStyleCnt="0"/></dgm:pt>
    <dgm:pt modelId="PN1" type="pres"><dgm:prSet presAssocID="N1" presStyleIdx="0" presStyleLbl="node1" presName="node" presStyleCnt="2"/></dgm:pt>
    <dgm:pt modelId="PS1" type="pres"><dgm:prSet presAssocID="S1" presName="sibTrans" presStyleCnt="0"/></dgm:pt>
    <dgm:pt modelId="PN2" type="pres"><dgm:prSet presAssocID="N2" presStyleIdx="1" presStyleLbl="node1" presName="node" presStyleCnt="2"/></dgm:pt>
  </dgm:ptLst>
  <dgm:cxnLst>
    <dgm:cxn modelId="H1" srcId="ROOT" destId="N1" srcOrd="0" destOrd="0" parTransId="P1" sibTransId="S1"/>
    <dgm:cxn modelId="H2" srcId="ROOT" destId="N2" srcOrd="1" destOrd="0" parTransId="P2" sibTransId="S2"/>
    <dgm:cxn modelId="PO_ROOT" type="presOf" srcId="ROOT" destId="P_ROOT" srcOrd="0" destOrd="0"/>
    <dgm:cxn modelId="PO1" type="presOf" srcId="N1" destId="PN1" srcOrd="0" destOrd="0"/>
    <dgm:cxn modelId="PO2" type="presOf" srcId="N2" destId="PN2" srcOrd="0" destOrd="0"/>
    <dgm:cxn modelId="PP1" type="presParOf" srcId="P_ROOT" destId="PN1" srcOrd="0" destOrd="0"/>
    <dgm:cxn modelId="PPS1" type="presParOf" srcId="P_ROOT" destId="PS1" srcOrd="1" destOrd="0"/>
    <dgm:cxn modelId="PP2" type="presParOf" srcId="P_ROOT" destId="PN2" srcOrd="2" destOrd="0"/>
  </dgm:cxnLst>
</dgm:dataModel>"#;

    fn element_by_model_id(xml: &str, local_name: &str, model_id: &str) -> String {
        let document = roxmltree::Document::parse(xml).unwrap();
        let element = document
            .descendants()
            .find(|node| is_dgm(*node, local_name) && node.attribute("modelId") == Some(model_id))
            .unwrap();
        xml[element.range()].to_string()
    }

    fn element_by_keep(xml: &str, local_name: &str, keep: &str) -> String {
        let document = roxmltree::Document::parse(xml).unwrap();
        let element = document
            .descendants()
            .find(|node| {
                node.is_element()
                    && node.tag_name().name() == local_name
                    && node.attribute(("urn:keep", "keep")) == Some(keep)
            })
            .unwrap();
        xml[element.range()].to_string()
    }

    #[test]
    fn parses_logical_tree_and_layout_without_exposing_presentation_points() {
        let model = parse_smartart_model(XML);
        assert_eq!(model["layout"], "urn:test:layout");
        let nodes = model["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 3);
        let alpha = nodes.iter().find(|node| node["id"] == "A").unwrap();
        assert_eq!(alpha["parentId"], "ROOT");
        assert_eq!(alpha["order"], 0);
        assert_eq!(alpha["text"], "Alpha");
        let run = &alpha["paragraphs"][0]["runs"][0];
        assert_eq!(run["font"], "Aptos");
        assert_eq!(run["fontScript"], "latin");
        assert_eq!(run["size"], 12.0);
        assert_eq!(run["bold"], true);
        assert_eq!(run["italic"], false);
        assert_eq!(run["underline"], "sng");
        assert_eq!(run["color"], "accent2");
        assert_eq!(run["alpha"], 0.7);
        assert!(!nodes.iter().any(|node| node["id"] == "PRES"));
    }

    #[test]
    fn applying_the_parsed_model_is_byte_exact() {
        let model = parse_smartart_model(XML);
        assert_eq!(apply_smartart_edit(XML, &model).unwrap(), XML);
    }

    #[test]
    fn equal_rich_text_update_is_byte_exact() {
        let model = parse_smartart_model(XML);
        let alpha = model["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["id"] == "A")
            .unwrap();
        let edited = apply_smartart_edit(
            XML,
            &json!({"updates":[{"id":"A", "paragraphs":alpha["paragraphs"].clone()}]}),
        )
        .unwrap();
        assert_eq!(edited, XML);
    }

    #[test]
    fn edits_one_node_run_without_flattening_unknown_rich_text_xml() {
        let model = parse_smartart_model(XML);
        let mut nodes = model["nodes"].clone();
        let alpha = nodes
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|node| node["id"] == "A")
            .unwrap();
        let run = &mut alpha["paragraphs"][0]["runs"][0];
        run["text"] = json!("Rich Alpha");
        run["font"] = json!("Calibri");
        run["size"] = json!(15.5);
        run["bold"] = json!(false);
        run["italic"] = json!(true);
        run["underline"] = json!("dbl");
        run["color"] = json!("#123456");
        run["alpha"] = json!(0.4);

        let untouched_b = element_by_model_id(XML, "pt", "B");
        let edited = apply_smartart_edit(XML, &json!({"nodes":nodes})).unwrap();
        assert_eq!(element_by_model_id(&edited, "pt", "B"), untouched_b);
        for sentinel in [
            "x:keep=\"text-container\"",
            "x:keep=\"paragraph\"",
            "<x:paragraphOpaque marker=\"keep\"/>",
            "x:keep=\"run\"",
            "x:keep=\"properties\"",
            "<x:colorTransform marker=\"keep\"/>",
            "<x:runPropertiesOpaque marker=\"keep\"/>",
            "<x:runOpaque marker=\"keep\"/>",
            "pitchFamily=\"34\"",
            "<a:endParaRPr lang=\"en-US\"/>",
        ] {
            assert!(edited.contains(sentinel), "missing {sentinel}");
        }
        assert!(
            !edited.contains("<a:tint val=\"20000\"/>"),
            "an explicit RGB edit must not retain the previous theme tint"
        );
        let reparsed = parse_smartart_model(&edited);
        let alpha = reparsed["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["id"] == "A")
            .unwrap();
        assert_eq!(alpha["text"], "Rich Alpha");
        let run = &alpha["paragraphs"][0]["runs"][0];
        assert_eq!(run["font"], "Calibri");
        assert_eq!(run["size"], 15.5);
        assert_eq!(run["bold"], false);
        assert_eq!(run["italic"], true);
        assert_eq!(run["underline"], "dbl");
        assert_eq!(run["color"], "#123456");
        assert_eq!(run["alpha"], 0.4);
    }

    #[test]
    fn smartart_run_and_paragraph_delete_keep_the_survivors_opaque_xml() {
        let model = parse_smartart_model(RICH_SEQUENCE_XML);
        let alpha = model["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["id"] == "A")
            .unwrap();
        assert_eq!(alpha["paragraphs"][0]["sourceIndex"], 0);
        assert_eq!(alpha["paragraphs"][1]["sourceIndex"], 1);
        assert_eq!(alpha["paragraphs"][0]["runs"][0]["sourceIndex"], 0);
        assert_eq!(alpha["paragraphs"][0]["runs"][1]["sourceIndex"], 1);

        let surviving_run = element_by_keep(RICH_SEQUENCE_XML, "r", "run-keep");
        let mut run_paragraphs = alpha["paragraphs"].clone();
        let runs = run_paragraphs[0]["runs"].as_array_mut().unwrap();
        runs.remove(0);
        run_paragraphs[0]["text"] = json!("Keep");
        let run_edited = apply_smartart_edit(
            RICH_SEQUENCE_XML,
            &json!({"updates":[{"id":"A", "paragraphs":run_paragraphs}]}),
        )
        .unwrap();
        assert_eq!(element_by_keep(&run_edited, "r", "run-keep"), surviving_run);
        assert!(!run_edited.contains("x:keep=\"run-drop\""));
        assert!(!run_edited.contains("<x:runOpaque marker=\"drop\"/>"));

        let surviving_paragraph = element_by_keep(RICH_SEQUENCE_XML, "p", "paragraph-keep");
        let mut legacy_paragraphs = alpha["paragraphs"].as_array().unwrap().clone();
        for paragraph in &mut legacy_paragraphs {
            paragraph.as_object_mut().unwrap().remove("sourceIndex");
            for run in paragraph["runs"].as_array_mut().unwrap() {
                run.as_object_mut().unwrap().remove("sourceIndex");
            }
        }
        legacy_paragraphs.remove(0);
        let paragraph_edited = apply_smartart_edit(
            RICH_SEQUENCE_XML,
            &json!({"updates":[{"id":"A", "paragraphs":legacy_paragraphs}]}),
        )
        .unwrap();
        assert_eq!(
            element_by_keep(&paragraph_edited, "p", "paragraph-keep"),
            surviving_paragraph
        );
        assert!(!paragraph_edited.contains("x:keep=\"paragraph-drop\""));
        assert!(!paragraph_edited.contains("x:keep=\"run-drop\""));
    }

    #[test]
    fn rich_text_update_combines_with_node_add_and_delete() {
        let model = parse_smartart_model(XML);
        let mut alpha_paragraphs = model["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["id"] == "A")
            .unwrap()["paragraphs"]
            .clone();
        alpha_paragraphs[0]["runs"][0]["bold"] = json!(false);
        alpha_paragraphs[0]["runs"][0]["text"] = json!("Alpha retained");
        let edited = apply_smartart_edit(
            XML,
            &json!({
                "updates":[{"id":"A", "paragraphs":alpha_paragraphs}],
                "addNodes":[{
                    "id":"rich-new", "parentId":"ROOT", "order":1, "kind":"node",
                    "paragraphs":[{"text":"New rich", "runs":[{
                        "kind":"r", "text":"New rich", "font":"Aptos Display",
                        "size":18, "bold":true, "italic":false, "underline":"none",
                        "color":"accent3", "alpha":0.8
                    }]}]
                }],
                "deleteIds":["B"]
            }),
        )
        .unwrap();
        let reparsed = parse_smartart_model(&edited);
        let nodes = reparsed["nodes"].as_array().unwrap();
        assert!(!nodes.iter().any(|node| node["id"] == "B"));
        let alpha = nodes.iter().find(|node| node["id"] == "A").unwrap();
        assert_eq!(alpha["text"], "Alpha retained");
        assert_eq!(alpha["paragraphs"][0]["runs"][0]["bold"], false);
        let added = nodes
            .iter()
            .find(|node| node["text"] == "New rich")
            .unwrap();
        let run = &added["paragraphs"][0]["runs"][0];
        assert_eq!(run["font"], "Aptos Display");
        assert_eq!(run["size"], 18.0);
        assert_eq!(run["bold"], true);
        assert_eq!(run["color"], "accent3");
        assert_eq!(run["alpha"], 0.8);
        assert!(edited.contains("x:unknown=\"keep-a\""));
        assert!(edited.contains("<x:opaque value=\"keep\"/>"));
    }

    #[test]
    fn edits_text_moves_and_adds_nodes_while_preserving_opaque_xml() {
        let edited = apply_smartart_edit(XML, &json!({
            "updates": [
                {"id":"A", "text":"A & <changed>", "order":1, "kind":"asst"},
                {"id":"B", "order":0}
            ],
            "addNodes": [
                {"id":"temp-child", "parentId":"A", "text":"New child", "order":0, "kind":"asst"}
            ]
        })).unwrap();

        assert!(edited.contains("x:unknown=\"keep-a\""));
        assert!(edited.contains("custom=\"stay\""));
        assert!(edited.contains("<x:opaque value=\"keep\"/>"));
        assert!(edited.contains("x:unknown=\"keep-cxn\""));
        assert!(edited.contains("x:unknown=\"keep-pres\""));
        assert!(edited.contains("A &amp; &lt;changed&gt;"));

        let model = parse_smartart_model(&edited);
        let nodes = model["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 4);
        let changed = nodes.iter().find(|node| node["id"] == "A").unwrap();
        assert_eq!(changed["kind"], "asst");
        assert_eq!(changed["text"], "A & <changed>");
        let added = nodes
            .iter()
            .find(|node| node["text"] == "New child")
            .unwrap();
        let generated = added["id"].as_str().unwrap();
        assert!(generated.starts_with('{') && generated.ends_with('}') && generated.len() == 38);
        assert_eq!(added["parentId"], "A");
        assert_eq!(added["kind"], "asst");
        assert_ne!(generated, "C1");

        let reparsed = roxmltree::Document::parse(&edited).unwrap();
        let root_children: Vec<_> = reparsed
            .descendants()
            .filter(|node| is_dgm(*node, "cxn") && node.attribute("srcId") == Some("ROOT"))
            .collect();
        assert_eq!(
            root_children
                .iter()
                .find(|node| node.attribute("destId") == Some("B"))
                .unwrap()
                .attribute("srcOrd"),
            Some("0")
        );
        assert_eq!(
            root_children
                .iter()
                .find(|node| node.attribute("destId") == Some("A"))
                .unwrap()
                .attribute("srcOrd"),
            Some("1")
        );
    }

    #[test]
    fn delete_cascades_but_keeps_unrelated_presentation_payload() {
        let with_child = apply_smartart_edit(
            XML,
            &json!({
                "addNodes": [{"id":"child", "parentId":"A", "text":"Child"}]
            }),
        )
        .unwrap();
        let model = parse_smartart_model(&with_child);
        let child_id = model["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["text"] == "Child")
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let deleted = apply_smartart_edit(&with_child, &json!({"deleteIds":["A"]})).unwrap();
        let after = parse_smartart_model(&deleted);
        assert!(
            !after["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|node| node["id"] == "A" || node["id"] == child_id)
        );
        assert!(!deleted.contains("modelId=\"PRES\""));
        assert!(deleted.contains("modelId=\"PRES_B\""));
        assert!(deleted.contains("modelId=\"PRES_KEEP\""));
        assert!(deleted.contains("x:sentinel=\"keep\""));
        assert!(deleted.contains("wholeDocumentOpaque"));
        assert!(!deleted.contains(&format!("destId=\"{child_id}\"")));
    }

    #[test]
    fn new_sibling_clones_the_nearest_presentation_template_and_pres_of_payload() {
        let original_b_point = element_by_model_id(XML, "pt", "PRES_B");
        let original_b_mapping = element_by_model_id(XML, "cxn", "CPB");
        let edited = apply_smartart_edit(
            XML,
            &json!({
                "addNodes": [{"id":"new-sibling", "parentId":"ROOT", "text":"Gamma", "order":2}]
            }),
        )
        .unwrap();
        let model = parse_smartart_model(&edited);
        let logical_id = model["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["text"] == "Gamma")
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let document = roxmltree::Document::parse(&edited).unwrap();
        let mapping = document
            .descendants()
            .find(|node| {
                is_dgm(*node, "cxn")
                    && node.attribute("type") == Some("presOf")
                    && node.attribute("srcId") == Some(logical_id.as_str())
            })
            .unwrap();
        let presentation_id = mapping.attribute("destId").unwrap();
        let presentation = document
            .descendants()
            .find(|node| is_dgm(*node, "pt") && node.attribute("modelId") == Some(presentation_id))
            .unwrap();
        let presentation_xml = &edited[presentation.range()];
        let mapping_xml = &edited[mapping.range()];
        assert_ne!(presentation_id, "PRES_B");
        assert!(presentation_xml.contains("x:template=\"beta\""));
        assert!(presentation_xml.contains(&format!("presAssocID=\"{logical_id}\"")));
        assert!(mapping_xml.contains("x:unknown=\"keep-pres-b\""));
        assert_eq!(
            element_by_model_id(&edited, "pt", "PRES_B"),
            original_b_point
        );
        assert_eq!(
            element_by_model_id(&edited, "cxn", "CPB"),
            original_b_mapping
        );

        let ids: Vec<&str> = document
            .descendants()
            .filter(Node::is_element)
            .filter_map(|node| node.attribute("modelId"))
            .collect();
        assert_eq!(ids.len(), ids.iter().copied().collect::<HashSet<_>>().len());
    }

    #[test]
    fn nested_child_uses_parent_presentation_target_and_office_transition_records() {
        let edited = apply_smartart_edit(
            XML,
            &json!({"addNodes":[{"id":"nested", "parentId":"A", "text":"Nested"}]}),
        )
        .unwrap();
        let model = parse_smartart_model(&edited);
        let logical_id = model["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["text"] == "Nested")
            .unwrap()["id"]
            .as_str()
            .unwrap();
        let document = roxmltree::Document::parse(&edited).unwrap();
        let logical_point = document
            .descendants()
            .find(|node| is_dgm(*node, "pt") && node.attribute("modelId") == Some(logical_id))
            .unwrap();
        assert_eq!(logical_point.attribute("type"), None);
        assert!(logical_point.children().any(|node| is_dgm(node, "spPr")));
        let text_body = logical_point
            .children()
            .find(|node| is_dgm(*node, "t"))
            .unwrap();
        assert!(text_body.children().any(|node| is_a(node, "bodyPr")));
        assert!(text_body.children().any(|node| is_a(node, "lstStyle")));

        let hierarchy = document
            .descendants()
            .find(|node| {
                is_dgm(*node, "cxn")
                    && node.attribute("srcId") == Some("A")
                    && node.attribute("destId") == Some(logical_id)
            })
            .unwrap();
        assert_eq!(hierarchy.attribute("type"), None);
        let hierarchy_id = hierarchy.attribute("modelId").unwrap();
        for (attribute, kind) in [("parTransId", "parTrans"), ("sibTransId", "sibTrans")] {
            let transition_id = hierarchy.attribute(attribute).unwrap();
            let transition = document
                .descendants()
                .find(|node| {
                    is_dgm(*node, "pt")
                        && node.attribute("modelId") == Some(transition_id)
                        && node.attribute("type") == Some(kind)
                        && node.attribute("cxnId") == Some(hierarchy_id)
                })
                .unwrap();
            assert!(transition.children().any(|node| is_dgm(node, "prSet")));
            assert!(transition.children().any(|node| is_dgm(node, "spPr")));
        }

        let mapping = document
            .descendants()
            .find(|node| {
                is_dgm(*node, "cxn")
                    && node.attribute("type") == Some("presOf")
                    && node.attribute("srcId") == Some(logical_id)
            })
            .unwrap();
        assert_eq!(mapping.attribute("destId"), Some("PRES"));
        assert_eq!(mapping.attribute("destOrd"), Some("1"));
        assert!(!document.descendants().any(|node| {
            is_dgm(node, "prSet") && node.attribute("presAssocID") == Some(logical_id)
        }));
    }

    #[test]
    fn office_root_sibling_updates_styles_and_inserts_sibling_transition_presentation() {
        let edited = apply_smartart_edit(
            OFFICE_ROOT_XML,
            &json!({"addNodes":[{"id":"third", "parentId":"ROOT", "text":"Three", "order":2}]}),
        )
        .unwrap();
        let model = parse_smartart_model(&edited);
        let logical_id = model["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["text"] == "Three")
            .unwrap()["id"]
            .as_str()
            .unwrap();
        let document = roxmltree::Document::parse(&edited).unwrap();

        let logical = document
            .descendants()
            .find(|node| is_dgm(*node, "pt") && node.attribute("modelId") == Some(logical_id))
            .unwrap();
        assert_eq!(logical.attribute("type"), None);
        assert!(logical.children().any(|node| is_dgm(node, "spPr")));
        let text_body = logical.children().find(|node| is_dgm(*node, "t")).unwrap();
        assert!(text_body.children().any(|node| is_a(node, "bodyPr")));
        assert!(text_body.children().any(|node| is_a(node, "lstStyle")));
        let hierarchy = document
            .descendants()
            .find(|node| is_dgm(*node, "cxn") && node.attribute("destId") == Some(logical_id))
            .unwrap();
        assert_eq!(hierarchy.attribute("type"), None);
        assert!(hierarchy.attribute("parTransId").is_some());
        assert!(hierarchy.attribute("sibTransId").is_some());

        let node_presentation = document
            .descendants()
            .filter(|node| is_dgm(*node, "pt") && node.attribute("type") == Some("pres"))
            .find(|node| {
                node.children().any(|child| {
                    is_dgm(child, "prSet")
                        && child.attribute("presAssocID") == Some(logical_id)
                        && child.attribute("presName") == Some("node")
                })
            })
            .unwrap();
        let node_properties = node_presentation
            .children()
            .find(|node| is_dgm(*node, "prSet"))
            .unwrap();
        assert_eq!(node_properties.attribute("presStyleIdx"), Some("2"));
        assert_eq!(node_properties.attribute("presStyleCnt"), Some("3"));
        for existing in ["PN1", "PN2"] {
            let point = document
                .descendants()
                .find(|node| is_dgm(*node, "pt") && node.attribute("modelId") == Some(existing))
                .unwrap();
            let properties = point
                .children()
                .find(|node| is_dgm(*node, "prSet"))
                .unwrap();
            assert_eq!(properties.attribute("presStyleCnt"), Some("3"));
        }

        let sibling_presentation = document
            .descendants()
            .filter(|node| is_dgm(*node, "pt") && node.attribute("type") == Some("pres"))
            .find(|node| {
                node.children().any(|child| {
                    is_dgm(child, "prSet")
                        && child.attribute("presAssocID") == Some("S2")
                        && child.attribute("presName") == Some("sibTrans")
                })
            })
            .unwrap();
        let sibling_presentation_id = sibling_presentation.attribute("modelId").unwrap();
        let sibling_parent = document
            .descendants()
            .find(|node| {
                is_dgm(*node, "cxn")
                    && node.attribute("type") == Some("presParOf")
                    && node.attribute("destId") == Some(sibling_presentation_id)
            })
            .unwrap();
        assert_eq!(sibling_parent.attribute("srcId"), Some("P_ROOT"));
        assert_eq!(sibling_parent.attribute("srcOrd"), Some("3"));
        let node_parent = document
            .descendants()
            .find(|node| {
                is_dgm(*node, "cxn")
                    && node.attribute("type") == Some("presParOf")
                    && node.attribute("destId") == node_presentation.attribute("modelId")
            })
            .unwrap();
        assert_eq!(node_parent.attribute("srcOrd"), Some("4"));
    }

    #[test]
    fn shared_presentation_point_survives_when_only_one_logical_owner_is_deleted() {
        let shared = XML.replace(
            "</dgm:cxnLst>",
            "<dgm:cxn modelId=\"CP_SHARED\" type=\"presOf\" srcId=\"B\" destId=\"PRES\" srcOrd=\"1\" destOrd=\"0\" x:shared=\"keep\"/></dgm:cxnLst>",
        );
        let edited = apply_smartart_edit(&shared, &json!({"deleteIds":["A"]})).unwrap();
        assert!(edited.contains("modelId=\"PRES\""));
        assert!(edited.contains("modelId=\"CP_SHARED\""));
        assert!(edited.contains("x:shared=\"keep\""));
        assert!(!edited.contains("modelId=\"CP\""));
    }

    #[test]
    fn delete_removes_exclusive_unmapped_presentation_descendants() {
        let with_presentation_child = XML
            .replace(
                "</dgm:ptLst>",
                "<dgm:pt modelId=\"PRES_CHILD\" type=\"pres\" x:exclusive=\"remove\"><dgm:prSet/></dgm:pt></dgm:ptLst>",
            )
            .replace(
                "</dgm:cxnLst>",
                "<dgm:cxn modelId=\"PRES_PARENT\" type=\"presParOf\" srcId=\"PRES\" destId=\"PRES_CHILD\" srcOrd=\"0\" destOrd=\"0\"/></dgm:cxnLst>",
            );
        let edited =
            apply_smartart_edit(&with_presentation_child, &json!({"deleteIds":["A"]})).unwrap();
        assert!(!edited.contains("modelId=\"PRES\""));
        assert!(!edited.contains("modelId=\"PRES_CHILD\""));
        assert!(!edited.contains("modelId=\"PRES_PARENT\""));
        assert!(!edited.contains("x:exclusive=\"remove\""));
    }

    #[test]
    fn creates_a_schema_minimal_presentation_mapping_without_a_template() {
        let without_presentation = r#"<dgm:dataModel xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><dgm:ptLst><dgm:pt modelId="ROOT" type="doc"><dgm:t><a:p><a:r><a:t>Root</a:t></a:r></a:p></dgm:t></dgm:pt></dgm:ptLst><dgm:cxnLst/></dgm:dataModel>"#;
        let edited = apply_smartart_edit(
            without_presentation,
            &json!({"addNodes":[{"id":"new", "parentId":"ROOT", "text":"Node"}]}),
        )
        .unwrap();
        let document = roxmltree::Document::parse(&edited).unwrap();
        let logical_id = parse_smartart_model(&edited)["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["text"] == "Node")
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let mapping = document
            .descendants()
            .find(|node| {
                is_dgm(*node, "cxn")
                    && node.attribute("type") == Some("presOf")
                    && node.attribute("srcId") == Some(logical_id.as_str())
            })
            .unwrap();
        let presentation_id = mapping.attribute("destId").unwrap();
        let presentation = document
            .descendants()
            .find(|node| {
                is_dgm(*node, "pt")
                    && node.attribute("type") == Some("pres")
                    && node.attribute("modelId") == Some(presentation_id)
            })
            .unwrap();
        assert!(edited[presentation.range()].contains(&format!("presAssocID=\"{logical_id}\"")));
    }

    #[test]
    fn move_and_reorder_leave_existing_presentation_mapping_byte_exact() {
        let point = element_by_model_id(XML, "pt", "PRES");
        let connection = element_by_model_id(XML, "cxn", "CP");
        let edited = apply_smartart_edit(
            XML,
            &json!({"updates":[{"id":"A", "parentId":"B", "order":0}]}),
        )
        .unwrap();
        assert_eq!(element_by_model_id(&edited, "pt", "PRES"), point);
        assert_eq!(element_by_model_id(&edited, "cxn", "CP"), connection);
    }

    #[test]
    fn top_level_reordering_changes_point_document_order() {
        let with_two_roots = XML.replace(
            "<dgm:pt modelId=\"A\"",
            "<dgm:pt modelId=\"ROOT2\" type=\"doc\" x:root=\"keep\"><dgm:prSet/><dgm:t><a:p><a:r><a:t>Second root</a:t></a:r></a:p></dgm:t></dgm:pt><dgm:pt modelId=\"A\"",
        );
        let edited = apply_smartart_edit(
            &with_two_roots,
            &json!({
                "updates": [
                    {"id":"ROOT", "order":1},
                    {"id":"ROOT2", "order":0}
                ]
            }),
        )
        .unwrap();

        assert!(
            edited.find("modelId=\"ROOT2\"").unwrap() < edited.find("modelId=\"ROOT\"").unwrap()
        );
        assert!(edited.contains("x:root=\"keep\""));
        assert!(edited.contains("x:opaque=\"presentation\""));
        let roots: Vec<Value> = parse_smartart_model(&edited)["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|node| node["parentId"].is_null())
            .cloned()
            .collect();
        assert_eq!(roots[0]["id"], "ROOT2");
        assert_eq!(roots[0]["order"], 0);
        assert_eq!(roots[1]["id"], "ROOT");
        assert_eq!(roots[1]["order"], 1);
    }

    #[test]
    fn rejects_cycles() {
        let error = apply_smartart_edit(
            XML,
            &json!({
                "updates": [
                    {"id":"A", "parentId":"B"},
                    {"id":"B", "parentId":"A"}
                ]
            }),
        )
        .unwrap_err();
        assert!(error.contains("cycle"));
    }
}
