//! A small, strict XML reader that keeps the text of WHITELISTED paths only.
//!
//! Everything else in `status.xml` (the Wi-Fi `key`, `ssid`, `infra_pass`, `infra_ssid`, the scan results, …)
//! is checked for well-formedness as the reader passes over it and is never copied anywhere: no string is
//! built from it, and no attribute value is kept.
//!
//! What is captured follows ElementTree's `root.findtext(path)`: the first element (document order) at that
//! path below the root; its text is the character data (references resolved, CDATA included, comments and
//! processing instructions skipped, line ends normalized) up to its first child element. A missing element and
//! an empty one both read as "".
//!
//! Well-formedness is checked as expat (Python's parser) would for this document type: UTF-8 (BOM allowed),
//! legal XML characters, names, matching tags, attribute syntax (no duplicates), the XML declaration, known
//! entity or character references, bound namespace prefixes, one root, nothing but whitespace, comments and
//! PIs outside it. As in ElementTree, an element in a namespace (prefixed, or under a default `xmlns`) never
//! matches a plain path. Not supported, unlike expat: entities declared in a DTD (an error here), and
//! encodings other than UTF-8/ASCII (a non-UTF-8 byte is an error here; Python would transcode).
//!
//! Note: the workspace lists `quick-xml`, but it could not be fetched in the build environment of this port
//! (no network); this module is self-contained so it can be swapped for a quick-xml event loop without
//! touching the callers.

use std::collections::HashMap;

/// `root/<name>` children read by `parse_status`.
const TOP: [&str; 8] =
    ["version", "heatversion", "uptime", "systemheat", "heaterstate", "currenttemp", "currenthumidity", "glycolstate"];
/// `root/heater1/<name>`.
const HEATER1: [&str; 10] = [
    "pidspeed",
    "battv",
    "runtime",
    "flametemp",
    "inlettemp",
    "outlettemp",
    "altitude",
    "glowpin",
    "dosingpump",
    "burnermotor",
];
/// `root/settings/<name>`.
const SETTINGS: [&str; 21] = [
    "fanspeed",
    "heatsources",
    "setpoint",
    "thermenabled",
    "fanenabled",
    "fanstate",
    "pumpstate",
    "furnacesrc",
    "electricsrc",
    "enginesrc",
    "floorsrc",
    "auxsrc",
    "cnstheat",
    "cnstheattimerval",
    "preheatsrc",
    "glycol",
    "electricenable",
    "engineenable",
    "preheatenable",
    "floorenable",
    "auxenable",
];

/// The XML is not well-formed (Python: `xml.etree.ElementTree.ParseError`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlError {
    pub msg: String,
    /// Byte offset in the document.
    pub offset: usize,
}

impl std::fmt::Display for XmlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (at byte {})", self.msg, self.offset)
    }
}

impl std::error::Error for XmlError {}

/// One `heater1-faults/fault`: the text of its first `name` and `value` children (`None`: no such child).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fault {
    pub name: Option<String>,
    pub value: Option<String>,
}

/// The whitelisted text of one `status.xml`.
#[derive(Debug, Clone, Default)]
pub struct Extract {
    fields: HashMap<(&'static str, &'static str), String>,
    pub faults: Vec<Fault>,
}

impl Extract {
    /// ElementTree `root.findtext("<parent>/<name>")` (parent "" for a root child), `None` if absent.
    pub fn findtext(&self, parent: &str, name: &str) -> Option<&str> {
        self.fields.iter().find(|((p, n), _)| *p == parent && *n == name).map(|(_, v)| v.as_str())
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Slot {
    Field(&'static str, &'static str),
    FaultName(usize),
    FaultValue(usize),
}

fn is_xml_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}')
}

fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n')
}

fn is_name_start(c: char) -> bool {
    matches!(c,
        ':' | 'A'..='Z' | '_' | 'a'..='z' | '\u{C0}'..='\u{D6}' | '\u{D8}'..='\u{F6}' | '\u{F8}'..='\u{2FF}'
        | '\u{370}'..='\u{37D}' | '\u{37F}'..='\u{1FFF}' | '\u{200C}'..='\u{200D}' | '\u{2070}'..='\u{218F}'
        | '\u{2C00}'..='\u{2FEF}' | '\u{3001}'..='\u{D7FF}' | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}'
        | '\u{10000}'..='\u{EFFFF}')
}

fn is_name_char(c: char) -> bool {
    is_name_start(c) || matches!(c, '-' | '.' | '0'..='9' | '\u{B7}' | '\u{300}'..='\u{36F}' | '\u{203F}'..='\u{2040}')
}

struct Reader<'a> {
    s: &'a str,
    i: usize,
}

impl<'a> Reader<'a> {
    fn err<T>(&self, msg: &str) -> Result<T, XmlError> {
        Err(XmlError { msg: msg.to_string(), offset: self.i })
    }

    fn rest(&self) -> &'a str {
        &self.s[self.i..]
    }

    fn skip_space(&mut self) -> bool {
        let n = self.rest().len() - self.rest().trim_start_matches(is_space).len();
        self.i += n;
        n > 0
    }

    fn name(&mut self) -> Result<&'a str, XmlError> {
        let r = self.rest();
        let mut chars = r.char_indices();
        match chars.next() {
            Some((_, c)) if is_name_start(c) => {}
            _ => return self.err("not well-formed (invalid token)"),
        }
        let end = chars.find(|(_, c)| !is_name_char(*c)).map_or(r.len(), |(i, _)| i);
        self.i += end;
        Ok(&r[..end])
    }

    /// Skip past `pat`, or fail with `msg`.
    fn skip_past(&mut self, pat: &str, msg: &str) -> Result<&'a str, XmlError> {
        match self.rest().find(pat) {
            Some(k) => {
                let body = &self.rest()[..k];
                self.i += k + pat.len();
                Ok(body)
            }
            None => self.err(msg),
        }
    }
}

/// Check (and, when `out` is given, resolve) the references in character data or an attribute value.
fn text(seg: &str, at: usize, mut out: Option<&mut String>) -> Result<(), XmlError> {
    let fail = |msg: &str, k: usize| Err(XmlError { msg: msg.to_string(), offset: at + k });
    let mut rest = seg;
    let mut base = 0;
    while let Some(k) = rest.find('&') {
        if let Some(o) = out.as_deref_mut() {
            o.push_str(&rest[..k]);
        }
        let after = &rest[k + 1..];
        let Some(semi) = after.find(';') else {
            return fail("not well-formed (invalid token)", base + k);
        };
        let r = &after[..semi];
        let c = if let Some(num) = r.strip_prefix('#') {
            let v = match num.strip_prefix('x') {
                Some(h) if !h.is_empty() && h.bytes().all(|b| b.is_ascii_hexdigit()) => u32::from_str_radix(h, 16).ok(),
                None if !num.is_empty() && num.bytes().all(|b| b.is_ascii_digit()) => num.parse().ok(),
                _ => None,
            };
            match v.and_then(char::from_u32).filter(|c| is_xml_char(*c)) {
                Some(c) => c,
                None => return fail("reference to invalid character number", base + k),
            }
        } else {
            match r {
                "lt" => '<',
                "gt" => '>',
                "amp" => '&',
                "apos" => '\'',
                "quot" => '"',
                _ if !r.is_empty() && r.chars().next().is_some_and(is_name_start) && r.chars().all(is_name_char) => {
                    return fail("undefined entity", base + k);
                }
                _ => return fail("not well-formed (invalid token)", base + k),
            }
        };
        if let Some(o) = out.as_deref_mut() {
            o.push(c);
        }
        rest = &after[semi + 1..];
        base += k + 1 + semi + 1;
    }
    if let Some(o) = out {
        o.push_str(rest);
    }
    Ok(())
}

/// XML end-of-line handling of literal text (not of character references): CR LF and a lone CR become LF.
fn normalized(seg: &str) -> std::borrow::Cow<'_, str> {
    if seg.contains('\r') { seg.replace("\r\n", "\n").replace('\r', "\n").into() } else { seg.into() }
}

/// The pseudo-attributes of an XML declaration (`<?xml` … `?>`): `version="1.x"`, then optionally
/// `encoding` and `standalone`, in that order, each after whitespace.
fn decl_ok(body: &str) -> bool {
    let (mut r, mut next, mut version) = (body, 0, false);
    loop {
        let t = r.trim_start_matches(is_space);
        if t.is_empty() {
            return version;
        }
        if t.len() == r.len() {
            return false; // no whitespace before the pseudo-attribute
        }
        let end = t.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(t.len());
        let pos = match &t[..end] {
            "version" => 0,
            "encoding" => 1,
            "standalone" => 2,
            _ => return false,
        };
        if pos < next || (pos > 0 && !version) {
            return false;
        }
        next = pos + 1;
        let Some(t) = t[end..].trim_start_matches(is_space).strip_prefix('=') else {
            return false;
        };
        let t = t.trim_start_matches(is_space);
        let Some(q) = t.chars().next().filter(|q| matches!(q, '"' | '\'')) else {
            return false;
        };
        let Some(k) = t[1..].find(q) else {
            return false;
        };
        let v = &t[1..1 + k];
        let good = match pos {
            0 => v.strip_prefix("1.").is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit())),
            1 => {
                v.starts_with(|c: char| c.is_ascii_alphabetic())
                    && v.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
            }
            _ => v == "yes" || v == "no",
        };
        if !good {
            return false;
        }
        version |= pos == 0;
        r = &t[2 + k..];
    }
}

/// One open element: its tag as written (for the end tag), the prefixes it declares, and whether a non-empty
/// default namespace is in scope.
struct Open<'a> {
    name: &'a str,
    prefixes: Vec<&'a str>,
    default_ns: bool,
}

/// Read `status.xml`, keeping the whitelisted text only.
pub fn extract(doc: &[u8]) -> Result<Extract, XmlError> {
    let s = std::str::from_utf8(doc)
        .map_err(|e| XmlError { msg: "not well-formed (invalid token)".into(), offset: e.valid_up_to() })?;
    let bom = if s.starts_with('\u{FEFF}') { '\u{FEFF}'.len_utf8() } else { 0 };
    if let Some((k, _)) = s.char_indices().skip(usize::from(bom > 0)).find(|(_, c)| !is_xml_char(*c)) {
        return Err(XmlError { msg: "not well-formed (invalid token)".into(), offset: k });
    }
    let mut rd = Reader { s, i: bom };
    let mut out = Extract::default();
    let mut stack: Vec<Open<'_>> = Vec::new();
    // the element names as ElementTree matches them: "" for a namespaced element (its tag is "{uri}name")
    let mut path: Vec<&str> = Vec::new();
    let mut root_done = false;
    let mut seen_doctype = false;
    // the element whose text is being collected, and its depth (stack length while it is open)
    let mut capture: Option<(Slot, usize, String)> = None;

    let finish = |out: &mut Extract, slot: Slot, text: String| match slot {
        Slot::Field(p, n) => {
            out.fields.insert((p, n), text);
        }
        Slot::FaultName(f) => out.faults[f].name = Some(text),
        Slot::FaultValue(f) => out.faults[f].value = Some(text),
    };

    while rd.i < s.len() {
        let at = rd.i;
        let r = rd.rest();
        if !r.starts_with('<') {
            let end = r.find('<').unwrap_or(r.len());
            let seg = &r[..end];
            if let Some(k) = seg.find("]]>") {
                return Err(XmlError { msg: "not well-formed (invalid token)".into(), offset: at + k });
            }
            if stack.is_empty() {
                if !seg.chars().all(is_space) {
                    return rd.err(if root_done { "junk after document element" } else { "syntax error" });
                }
                text(seg, at, None)?;
            } else if let Some((_, _, buf)) = capture.as_mut().filter(|(_, d, _)| *d == stack.len()) {
                text(&normalized(seg), at, Some(buf))?;
            } else {
                text(seg, at, None)?;
            }
            rd.i += end;
            continue;
        }
        if r.starts_with("<?") {
            rd.i += 2;
            let target = rd.name()?;
            if target.eq_ignore_ascii_case("xml") && (target != "xml" || at != bom) {
                return rd.err("XML or text declaration not at start of entity");
            }
            let body = rd.skip_past("?>", "unclosed token")?;
            if target == "xml" && !decl_ok(body) {
                return rd.err("XML declaration not well-formed");
            }
            continue;
        }
        if r.starts_with("<!--") {
            rd.i += 4;
            let body = rd.skip_past("-->", "unclosed token")?;
            if body.contains("--") || body.ends_with('-') {
                return rd.err("not well-formed (invalid token)");
            }
            continue;
        }
        if r.starts_with("<![CDATA[") {
            if stack.is_empty() {
                return rd.err("not well-formed (invalid token)");
            }
            rd.i += 9;
            let body = rd.skip_past("]]>", "unclosed CDATA section")?;
            if let Some((_, _, buf)) = capture.as_mut().filter(|(_, d, _)| *d == stack.len()) {
                buf.push_str(&normalized(body));
            }
            continue;
        }
        if r.starts_with("<!DOCTYPE") {
            if seen_doctype || root_done || !stack.is_empty() {
                return rd.err("syntax error");
            }
            seen_doctype = true;
            rd.i += 9;
            let (mut quote, mut bracket) = (None, 0usize);
            loop {
                let Some(c) = rd.rest().chars().next() else {
                    return rd.err("unclosed token");
                };
                rd.i += c.len_utf8();
                match (quote, c) {
                    (Some(q), c) if c == q => quote = None,
                    (Some(_), _) => {}
                    (None, '"' | '\'') => quote = Some(c),
                    (None, '[') => bracket += 1,
                    (None, ']') => bracket = bracket.saturating_sub(1),
                    (None, '>') if bracket == 0 => break,
                    _ => {}
                }
            }
            continue;
        }
        if r.starts_with("</") {
            rd.i += 2;
            let name = rd.name()?;
            rd.skip_space();
            if !rd.rest().starts_with('>') {
                return rd.err("not well-formed (invalid token)");
            }
            rd.i += 1;
            if stack.last().map(|o| o.name) != Some(name) {
                return Err(XmlError { msg: "mismatched tag".into(), offset: at });
            }
            if let Some((slot, _, buf)) = capture.take_if(|(_, d, _)| *d == stack.len()) {
                finish(&mut out, slot, buf);
            }
            stack.pop();
            path.pop();
            root_done = stack.is_empty();
            continue;
        }
        // a start tag or an empty-element tag
        if root_done {
            return rd.err("junk after document element");
        }
        rd.i += 1;
        let name = rd.name()?;
        let mut attrs: Vec<&str> = Vec::new();
        let mut prefixes: Vec<&str> = Vec::new();
        let mut default_decl: Option<bool> = None;
        let empty = loop {
            let spaced = rd.skip_space();
            let r = rd.rest();
            if r.starts_with("/>") {
                rd.i += 2;
                break true;
            }
            if r.starts_with('>') {
                rd.i += 1;
                break false;
            }
            if !spaced {
                return rd.err("not well-formed (invalid token)");
            }
            let a = rd.name()?;
            if attrs.contains(&a) {
                return rd.err("duplicate attribute");
            }
            attrs.push(a);
            rd.skip_space();
            if !rd.rest().starts_with('=') {
                return rd.err("not well-formed (invalid token)");
            }
            rd.i += 1;
            rd.skip_space();
            let q = match rd.rest().chars().next() {
                Some(q @ ('"' | '\'')) => q,
                _ => return rd.err("not well-formed (invalid token)"),
            };
            rd.i += 1;
            let vat = rd.i;
            let Some(k) = rd.rest().find(q) else {
                return rd.err("unclosed token");
            };
            let value = &rd.rest()[..k];
            if value.contains('<') {
                return rd.err("not well-formed (invalid token)");
            }
            text(value, vat, None)?; // checked, never kept
            if a == "xmlns" {
                default_decl = Some(!value.is_empty()); // a namespace URI, not data
            } else if let Some(p) = a.strip_prefix("xmlns:") {
                if value.is_empty() {
                    return rd.err("must not undeclare prefix");
                }
                prefixes.push(p);
            }
            rd.i += k + 1;
        };
        // Namespaces as expat applies them for ElementTree: every prefix must be bound, and a prefixed or
        // default-namespaced element never matches a plain path.
        let bound = |p: &str| p == "xml" || prefixes.contains(&p) || stack.iter().any(|o| o.prefixes.contains(&p));
        fn prefix_of(n: &str) -> Option<&str> {
            n.split_once(':').map(|(p, _)| p)
        }
        if prefix_of(name).is_some_and(|p| !bound(p))
            || attrs
                .iter()
                .any(|a| *a != "xmlns" && !a.starts_with("xmlns:") && prefix_of(a).is_some_and(|p| !bound(p)))
        {
            return Err(XmlError { msg: "unbound prefix".into(), offset: at });
        }
        let default_ns = default_decl.unwrap_or_else(|| stack.last().is_some_and(|o| o.default_ns));
        let eff = if prefix_of(name).is_none() && !default_ns { name } else { "" };

        // A child element ends its parent's text (ElementTree `.text` stops at the first child).
        if let Some((slot, _, buf)) = capture.take_if(|(_, d, _)| *d == stack.len()) {
            finish(&mut out, slot, buf);
        }
        let slot = match path.as_slice() {
            [_] => TOP.iter().find(|n| **n == eff).map(|n| Slot::Field("", n)),
            [_, "heater1"] => HEATER1.iter().find(|n| **n == eff).map(|n| Slot::Field("heater1", n)),
            [_, "settings"] => SETTINGS.iter().find(|n| **n == eff).map(|n| Slot::Field("settings", n)),
            [_, "heater1-faults"] if eff == "fault" => {
                out.faults.push(Fault::default());
                None
            }
            [_, "heater1-faults", "fault"] => {
                let f = out.faults.len() - 1;
                match eff {
                    "name" if out.faults[f].name.is_none() => Some(Slot::FaultName(f)),
                    "value" if out.faults[f].value.is_none() => Some(Slot::FaultValue(f)),
                    _ => None,
                }
            }
            _ => None,
        };
        // first occurrence only
        let slot = slot.filter(|sl| match sl {
            Slot::Field(p, n) => !out.fields.contains_key(&(*p, *n)),
            _ => true,
        });
        stack.push(Open { name, prefixes, default_ns });
        path.push(eff);
        match (slot, empty) {
            (Some(sl), true) => finish(&mut out, sl, String::new()),
            (Some(sl), false) => capture = Some((sl, stack.len(), String::new())),
            (None, _) => {}
        }
        if empty {
            stack.pop();
            path.pop();
            root_done = stack.is_empty();
        }
    }
    if !stack.is_empty() {
        return rd.err("no element found");
    }
    if !root_done {
        return rd.err("no element found");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(x: &str) -> Extract {
        extract(x.as_bytes()).unwrap()
    }

    #[test]
    fn text_like_elementtree() {
        let e = ok("<r><version>a<!-- c -->b&amp;&#x41;<![CDATA[<x>]]>\r\nz<sub/>tail</version></r>");
        assert_eq!(e.findtext("", "version"), Some("ab&A<x>\nz"));
        let e = ok("<r><settings/><settings><setpoint>1</setpoint><setpoint>2</setpoint></settings></r>");
        assert_eq!(e.findtext("settings", "setpoint"), Some("1"));
        let e = ok("<r><heater1><heaterstate>9</heaterstate></heater1></r>");
        assert_eq!(e.findtext("", "heaterstate"), None); // only a direct child of the root counts
        let e = ok("<r><uptime/><uptime>5</uptime></r>");
        assert_eq!(e.findtext("", "uptime"), Some("")); // the first match wins, even when empty
        let e = ok("<?xml version='1.0'?>\n<!DOCTYPE r>\n<r a='1' b=\"x>y\">\
             <heater1-faults><fault><name>AF</name></fault></heater1-faults></r>\n");
        assert_eq!(e.faults, vec![Fault { name: Some("AF".into()), value: None }]);
    }

    #[test]
    fn rejects_what_expat_rejects() {
        for bad in [
            "",
            "<r>",
            "<r></s>",
            "<r/><r/>",
            "<r/>junk",
            "x<r/>",
            "<r>&nope;</r>",
            "<r>& </r>",
            "<r a='1' a='2'/>",
            "<r a=1/>",
            "<r>\u{1}</r>",
            "<r><!-- a -- b --></r>",
            "<r/><?xml version='1.0'?>",
            "<r>]]></r>",
            "<r>&#0;</r>",
        ] {
            assert!(extract(bad.as_bytes()).is_err(), "{bad:?}");
        }
        assert!(extract(b"<r>\xff</r>").is_err());
        for bad in [
            "<r><a:version>1</a:version></r>",
            "<r a:b='1'/>",
            "<?xml version='2.0'?><r/>",
            "<?xml?><r/>",
            "<?xml encoding='utf-8'?><r/>",
            "<r><version xmlns:p=''>1</version></r>",
        ] {
            assert!(extract(bad.as_bytes()).is_err(), "{bad:?}");
        }
    }

    /// Expected values from Python's `ET.fromstring(doc).findtext(...)`.
    #[test]
    fn namespaces_and_declarations_like_elementtree() {
        let e = ok("<r xmlns='u'><version>1</version></r>");
        assert_eq!(e.findtext("", "version"), None); // its tag is "{u}version"
        let e = ok("<r xmlns='u'><version xmlns=''>1</version></r>");
        assert_eq!(e.findtext("", "version"), Some("1"));
        let e = ok("<r xmlns:a='u'><a:version>1</a:version><version>2</version></r>");
        assert_eq!(e.findtext("", "version"), Some("2"));
        let e = ok("<r><settings xmlns='u'><setpoint xmlns=''>3</setpoint></settings></r>");
        assert_eq!(e.findtext("settings", "setpoint"), None);
        let e = ok("<r><version xml:lang='en'>1</version></r>");
        assert_eq!(e.findtext("", "version"), Some("1"));
        let e = ok("<?xml  version = '1.0'  ?><r a='x>y' b=\"]]>\"><version>a&#13;b</version></r>");
        assert_eq!(e.findtext("", "version"), Some("a\rb")); // a character reference is not line-end normalized
        assert!(extract("\u{FEFF}<?xml version=\"1.0\" encoding='utf-8' standalone='yes'?><r/>".as_bytes()).is_ok());
    }
}
