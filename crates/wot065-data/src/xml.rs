//! Minimal XML tree reader for BigWorld resource files.
//!
//! WoT 0.6.5 ships `res/scripts/item_defs/...` as **packed XML** (BigWorld "XML binary",
//! magic `45 4E A1 62`). We decode those with the MIT `wg-toolkit` crate
//! (`wgtk::pxml::from_bytes`, `vendor/wg-toolkit-rs/wg-toolkit/src/pxml/de.rs`) and expose
//! a tiny uniform tree. A plain-text XML parser is included as a fallback so the same code
//! can read the pre-unpacked development copies (`.scratch/research/unpacked/.../*.xml.xml`)
//! and any future unpacked dumps.

use std::fmt;
use std::path::Path;

use wgtk::pxml::Value as PxmlValue;

/// A parsed XML element. `value` is the element's direct text content (empty for
/// container elements); `children` preserves document order and duplicate names, which
/// matters because `item_defs` uses repeated tags (e.g. one child per component).
#[derive(Debug, Clone, Default)]
pub struct XmlNode {
    pub name: String,
    pub value: String,
    pub children: Vec<XmlNode>,
}

#[derive(Debug)]
pub struct XmlError(pub String);

impl fmt::Display for XmlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "xml error: {}", self.0)
    }
}

impl std::error::Error for XmlError {}

impl XmlNode {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: String::new(),
            children: Vec::new(),
        }
    }

    /// First direct child with this name.
    pub fn child(&self, name: &str) -> Option<&XmlNode> {
        self.children.iter().find(|c| c.name == name)
    }

    /// All direct children with this name, in document order.
    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a XmlNode> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }

    /// First child, whatever its name (used to pick the stock component of a section).
    pub fn first_child(&self) -> Option<&XmlNode> {
        self.children.first()
    }

    /// Trimmed direct text content.
    pub fn text(&self) -> &str {
        self.value.trim()
    }

    pub fn parse_i64(&self) -> Option<i64> {
        let t = self.text();
        t.parse().ok().or_else(|| t.parse::<f64>().ok().map(|f| f as i64))
    }

    pub fn parse_f64(&self) -> Option<f64> {
        self.text().parse().ok()
    }

    /// Whitespace-separated tokens of the text content (e.g. `<tags>`).
    pub fn parse_list(&self) -> Vec<&str> {
        self.text().split_whitespace().collect()
    }

    /// Depth-first walk starting at this node (including it).
    pub fn walk<'a>(&'a self, f: &mut impl FnMut(&'a XmlNode)) {
        f(self);
        for c in &self.children {
            c.walk(f);
        }
    }
}

/// Read a file that is either BigWorld packed XML or plain XML.
pub fn read_xml(path: &Path) -> Result<XmlNode, XmlError> {
    let bytes = std::fs::read(path).map_err(|e| XmlError(format!("{}: {e}", path.display())))?;
    parse_xml_bytes(&bytes)
}

/// Parse BigWorld packed XML if the magic matches, plain XML otherwise.
pub fn parse_xml_bytes(bytes: &[u8]) -> Result<XmlNode, XmlError> {
    if bytes.starts_with(wgtk::pxml::MAGIC) {
        parse_packed(bytes)
    } else if bytes.starts_with(b"<") || bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
        parse_plain(bytes)
    } else {
        Err(XmlError(format!(
            "unrecognised file: magic {:02x?}",
            &bytes[..bytes.len().min(8)]
        )))
    }
}

/// Decode BigWorld packed XML via `wg-toolkit`.
pub fn parse_packed(bytes: &[u8]) -> Result<XmlNode, XmlError> {
    let root = wgtk::pxml::from_bytes(bytes).map_err(|e| XmlError(format!("packed xml: {e}")))?;
    Ok(convert_element("", &root))
}

fn convert_element(name: &str, element: &wgtk::pxml::Element) -> XmlNode {
    let mut node = XmlNode {
        name: name.to_string(),
        value: scalar_to_string(&element.value),
        children: Vec::with_capacity(element.len()),
    };
    for (child_name, child_value) in element.iter_children_all() {
        node.children
            .push(convert_value(child_name, child_value));
    }
    node
}

fn convert_value(name: &str, value: &PxmlValue) -> XmlNode {
    match value {
        PxmlValue::Element(element) => convert_element(name, element),
        other => XmlNode {
            name: name.to_string(),
            value: scalar_to_string(other),
            children: Vec::new(),
        },
    }
}

fn scalar_to_string(value: &PxmlValue) -> String {
    match value {
        PxmlValue::String(s) => s.clone(),
        PxmlValue::Integer(i) => i.to_string(),
        PxmlValue::Boolean(b) => {
            if *b {
                "true".to_string()
            } else {
                "false".to_string()
            }
        }
        PxmlValue::Vector(vector) => {
            let mut s = String::new();
            for (i, f) in vector.iter().enumerate() {
                if i != 0 {
                    s.push(' ');
                }
                s.push_str(&format_float(*f));
            }
            s
        }
        PxmlValue::Element(e) => scalar_to_string(&e.value),
    }
}

fn format_float(f: f32) -> String {
    if f == f.trunc() && f.abs() < 1e7 {
        format!("{}", f as i64)
    } else {
        let mut s = format!("{f}");
        if !s.contains('.') && !s.contains('e') {
            s.push_str(".0");
        }
        s
    }
}

/// Very small plain-text XML parser: elements, nested elements, text content, comments and
/// the XML declaration. Attributes (unused by item_defs) are ignored. This is deliberately
/// not a general XML implementation - it only serves to read WG resource files.
pub fn parse_plain(bytes: &[u8]) -> Result<XmlNode, XmlError> {
    let text = String::from_utf8_lossy(bytes);
    let bytes = text.as_bytes();
    let mut parser = PlainParser {
        bytes,
        pos: 0,
        nodes: Vec::new(),
        root: XmlNode::new(""),
    };
    parser.parse_document()?;
    Ok(parser.root)
}

struct PlainParser<'a> {
    bytes: &'a [u8],
    pos: usize,
    nodes: Vec<XmlNode>,
    root: XmlNode,
}

impl<'a> PlainParser<'a> {
    fn err<T>(&self, msg: &str) -> Result<T, XmlError> {
        Err(XmlError(format!("{msg} at byte {}", self.pos)))
    }

    fn starts_with(&self, s: &str) -> bool {
        self.bytes[self.pos..].starts_with(s.as_bytes())
    }

    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len() && (self.bytes[self.pos] as char).is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn parse_document(&mut self) -> Result<(), XmlError> {
        loop {
            self.skip_ws();
            if self.pos >= self.bytes.len() {
                break;
            }
            if self.bytes[self.pos] != b'<' {
                // stray text outside the root element
                self.skip_text();
                continue;
            }
            if self.starts_with("<?") {
                self.skip_until("?>")?;
            } else if self.starts_with("<!--") {
                self.skip_until("-->")?;
            } else if self.starts_with("<!") {
                self.skip_until(">")?;
            } else if self.starts_with("</") {
                return self.err("unexpected closing tag");
            } else {
                let node = self.parse_element()?;
                self.nodes.push(node);
            }
        }
        if self.nodes.len() != 1 {
            return self.err(&format!("expected exactly one root element, got {}", self.nodes.len()));
        }
        // BigWorld packed XML has an *anonymous* document root: the top-level tags of the
        // document are the element's children. The unpacked development copies wrap those
        // children in a synthetic `<root>`, so strip it to keep both trees identical.
        let root = self.nodes.pop().unwrap();
        self.root = XmlNode {
            name: String::new(),
            value: String::new(),
            children: root.children,
        };
        Ok(())
    }

    fn skip_text(&mut self) {
        while self.pos < self.bytes.len() && self.bytes[self.pos] != b'<' {
            self.pos += 1;
        }
    }

    fn skip_until(&mut self, pat: &str) -> Result<(), XmlError> {
        match find(&self.bytes[self.pos..], pat.as_bytes()) {
            Some(i) => {
                self.pos += i + pat.len();
                Ok(())
            }
            None => self.err("unterminated token"),
        }
    }

    fn read_name(&mut self) -> &'a str {
        let start = self.pos;
        while self.pos < self.bytes.len()
            && !matches!(
                self.bytes[self.pos],
                b' ' | b'\t' | b'\r' | b'\n' | b'/' | b'>'
            )
        {
            self.pos += 1;
        }
        std::str::from_utf8(&self.bytes[start..self.pos]).unwrap_or("")
    }

    fn parse_element(&mut self) -> Result<XmlNode, XmlError> {
        debug_assert_eq!(self.bytes[self.pos], b'<');
        self.pos += 1;
        let name = self.read_name().to_string();
        if name.is_empty() {
            return self.err("empty element name");
        }
        // Skip attributes.
        while self.pos < self.bytes.len()
            && self.bytes[self.pos] != b'>'
            && !self.starts_with("/>")
        {
            self.pos += 1;
        }
        let mut node = XmlNode::new(name);
        if self.starts_with("/>") {
            self.pos += 2;
            return Ok(node);
        }
        if self.pos >= self.bytes.len() || self.bytes[self.pos] != b'>' {
            return self.err("unterminated opening tag");
        }
        self.pos += 1;
        let mut value = String::new();
        loop {
            if self.pos >= self.bytes.len() {
                return self.err("unexpected eof inside element");
            }
            if self.bytes[self.pos] != b'<' {
                let start = self.pos;
                self.skip_text();
                value.push_str(&String::from_utf8_lossy(&self.bytes[start..self.pos]));
                continue;
            }
            if self.starts_with("</") {
                self.pos += 2;
                let close_name = self.read_name().to_string();
                while self.pos < self.bytes.len() && self.bytes[self.pos] != b'>' {
                    self.pos += 1;
                }
                if self.pos >= self.bytes.len() {
                    return self.err("unterminated closing tag");
                }
                self.pos += 1;
                if close_name != node.name {
                    return self.err(&format!(
                        "mismatched closing tag: expected </{}>, got </{}>",
                        node.name, close_name
                    ));
                }
                break;
            } else if self.starts_with("<!--") {
                self.skip_until("-->")?;
            } else if self.starts_with("<?") {
                self.skip_until("?>")?;
            } else {
                node.children.push(self.parse_element()?);
            }
        }
        node.value = value.trim().to_string();
        Ok(node)
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_basic() {
        let src = br#"<?xml version="1.0"?>
            <!-- comment -->
            <root>
                <ids><A>1</A><B>2</B></ids>
                <veh><id>7</id><tags>light tank</tags><v>shared<sub>x</sub></v></veh>
                <self/>
            </root>"#;
        let root = parse_plain(src).unwrap();
        assert_eq!(root.name, "");
        assert_eq!(root.child("ids").unwrap().children.len(), 2);
        assert_eq!(root.child("veh").unwrap().child("id").unwrap().parse_i64(), Some(7));
        assert_eq!(
            root.child("veh").unwrap().child("tags").unwrap().parse_list(),
            vec!["light", "tank"]
        );
        let v = root.child("veh").unwrap().child("v").unwrap();
        assert_eq!(v.text(), "shared");
        assert_eq!(v.child("sub").unwrap().text(), "x");
        assert!(root.child("self").unwrap().children.is_empty());
    }

    #[test]
    fn plain_unpacked_item_def_is_parseable() {
        // Optional local check: read a pre-unpacked development copy if present.
        let path = crate::testutil::repo_root()
            .join(".scratch/research/unpacked/res/scripts/item_defs/vehicles/ussr/list.xml.xml");
        if !path.exists() {
            return;
        }
        let root = read_xml(&path).unwrap();
        assert!(root.children.len() > 40, "ussr list has ~45 vehicles");
        assert_eq!(root.child("T-34").unwrap().child("level").unwrap().parse_i64(), Some(5));
    }
}
