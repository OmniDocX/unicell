//! CSS `<style>` cascade support: matches selectors against SVG elements via
//! the `simplecss` crate and returns the declared properties for a node.

use std::collections::HashMap;

use roxmltree::Node;
use simplecss::{AttributeOperator, Element, PseudoClass, StyleSheet};

/// Thin wrapper making a `roxmltree::Node` matchable by `simplecss`.
#[derive(Clone, Copy)]
pub struct NodeRef<'a, 'input> {
    node: Node<'a, 'input>,
}

impl<'a, 'input> NodeRef<'a, 'input> {
    pub fn new(node: Node<'a, 'input>) -> Self {
        Self { node }
    }
}

impl<'a, 'input> Element for NodeRef<'a, 'input> {
    fn parent_element(&self) -> Option<Self> {
        self.node.parent_element().map(NodeRef::new)
    }

    fn prev_sibling_element(&self) -> Option<Self> {
        let mut sib = self.node.prev_sibling();
        while let Some(n) = sib {
            if n.is_element() {
                return Some(NodeRef::new(n));
            }
            sib = n.prev_sibling();
        }
        None
    }

    fn has_local_name(&self, name: &str) -> bool {
        self.node.tag_name().name() == name
    }

    fn attribute_matches(&self, local_name: &str, operator: AttributeOperator) -> bool {
        match self.node.attribute(local_name) {
            Some(value) => operator.matches(value),
            None => false,
        }
    }

    fn pseudo_class_matches(&self, _class: PseudoClass) -> bool {
        // Structural/state pseudo-classes are not meaningful for static
        // vector conversion; treat them as non-matching.
        false
    }
}

/// Collect all `<style>` element text in the document, concatenated.
pub fn collect_style_text(root: &Node) -> String {
    let mut css = String::new();
    for node in root.descendants() {
        if node.is_element() && node.tag_name().name() == "style" {
            for child in node.children() {
                if let Some(t) = child.text() {
                    css.push_str(t);
                    css.push('\n');
                }
            }
        }
    }
    css
}

/// Resolve the CSS properties applying to `node`, honoring selector
/// specificity (later, more specific rules win). Returns property name/value
/// pairs; `!important` is treated as a high-specificity win.
pub fn matched_properties(sheet: &StyleSheet, node: &Node) -> HashMap<String, String> {
    let target = NodeRef::new(*node);
    // (specificity, order) -> declarations, applied in ascending order.
    let mut ranked: Vec<([u8; 3], usize, &str, &str, bool)> = Vec::new();
    for (order, rule) in sheet.rules.iter().enumerate() {
        if rule.selector.matches(&target) {
            let spec = rule.selector.specificity();
            for decl in &rule.declarations {
                ranked.push((spec, order, decl.name, decl.value, decl.important));
            }
        }
    }
    // Sort so that the winning declaration is applied last: by important,
    // then specificity, then document order.
    ranked.sort_by(|a, b| {
        a.4.cmp(&b.4) // important last
            .then(a.0.cmp(&b.0)) // higher specificity last
            .then(a.1.cmp(&b.1)) // later rule last
    });
    let mut out = HashMap::new();
    for (_, _, name, value, _) in ranked {
        out.insert(name.to_string(), value.trim().to_string());
    }
    out
}
