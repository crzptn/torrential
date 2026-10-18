//! RSS items as flat lists of child elements, which is all Nyaa, Torznab, and the
//! Academic Torrents catalog need. Element names are matched without their
//! namespace prefix, so `nyaa:seeders` is `seeders`.

use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

#[derive(Default, Debug)]
pub(crate) struct Item {
    fields: Vec<(String, String)>,
    /// Torznab `<torznab:attr name=… value=…/>` pairs.
    attrs: Vec<(String, String)>,
    pub enclosure: Option<String>,
}

impl Item {
    pub fn get<'a>(&'a self, name: &'a str) -> Option<&'a str> {
        self.all(name).next()
    }

    pub fn all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> {
        self.fields.iter().filter(move |(k, _)| k == name).map(|(_, v)| v.as_str()).filter(|v| !v.is_empty())
    }

    pub fn attr<'a>(&'a self, name: &'a str) -> Option<&'a str> {
        self.attrs(name).next()
    }

    pub fn attrs<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> {
        self.attrs.iter().filter(move |(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

/// Fails on a document whose root is not `<rss>`, such as an anti-bot HTML page, and
/// on a Torznab `<error>` document, whose description becomes the reason.
pub(crate) fn items(body: &[u8]) -> Result<Vec<Item>, String> {
    let text = String::from_utf8_lossy(body);
    // Not `trim_text`: it trims around entity references, so "Tom &amp; Jerry"
    // would lose its spaces. Fields are trimmed whole instead.
    let mut reader = Reader::from_str(&text);
    let mut items = Vec::new();
    let mut item: Option<Item> = None;
    let mut field: Option<(String, String)> = None;
    let mut seen_root = false;
    loop {
        let event = reader.read_event().map_err(|e| format!("malformed XML: {e}"))?;
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let name = local(e);
                if !seen_root {
                    seen_root = true;
                    match name.as_str() {
                        "rss" => {}
                        "error" => {
                            let code = attr(e, "code").unwrap_or_default();
                            let description = attr(e, "description").unwrap_or_default();
                            return Err(format!("indexer error {code}: {description}"));
                        }
                        other => return Err(format!("expected an RSS feed, got <{other}>")),
                    }
                }
                let empty = matches!(event, Event::Empty(_));
                match (&mut item, name.as_str()) {
                    (None, "item") if !empty => item = Some(Item::default()),
                    (Some(it), "attr") => {
                        if let (Some(k), Some(v)) = (attr(e, "name"), attr(e, "value")) {
                            it.attrs.push((k, v));
                        }
                    }
                    (Some(it), "enclosure") => it.enclosure = attr(e, "url"),
                    (Some(_), _) if !empty => field = Some((name, String::new())),
                    _ => {}
                }
            }
            Event::Text(t) => push_text(&mut field, &t),
            Event::CData(t) => push_text(&mut field, &t),
            Event::GeneralRef(r) => {
                let resolved = match r.resolve_char_ref() {
                    Ok(Some(c)) => Some(c),
                    _ => match &*r {
                        "amp" => Some('&'),
                        "lt" => Some('<'),
                        "gt" => Some('>'),
                        "quot" => Some('"'),
                        "apos" => Some('\''),
                        _ => None,
                    },
                };
                if let (Some(c), Some((_, value))) = (resolved, &mut field) {
                    value.push(c);
                }
            }
            Event::End(e) => {
                let name = e.local_name().as_ref().to_string();
                if name == "item" {
                    items.extend(item.take());
                } else if let (Some(it), Some((key, value))) = (&mut item, field.take()) {
                    it.fields.push((key, value.trim().to_string()));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !seen_root {
        return Err("empty document".into());
    }
    Ok(items)
}

fn push_text(field: &mut Option<(String, String)>, text: &str) {
    if let Some((_, value)) = field {
        value.push_str(text);
    }
}

fn local(e: &BytesStart<'_>) -> String {
    e.local_name().as_ref().to_string()
}

fn attr(e: &BytesStart<'_>, name: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref() == name)
        .and_then(|a| a.normalized_value(XmlVersion::default()).ok().map(|v| v.into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_fields_attrs_and_entities() {
        let xml = br#"<?xml version="1.0"?><rss xmlns:t="x"><channel><title>feed</title>
            <item><title>Night &amp; Day&#33;</title><t:attr name="seeders" value="4"/>
            <enclosure url="https://example.org/a.torrent" length="1"/><description><![CDATA[<b>hi</b>]]></description></item>
            </channel></rss>"#;
        let items = items(xml).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].get("title"), Some("Night & Day!"));
        assert_eq!(items[0].attr("seeders"), Some("4"));
        assert_eq!(items[0].enclosure.as_deref(), Some("https://example.org/a.torrent"));
        assert_eq!(items[0].get("description"), Some("<b>hi</b>"));
    }

    #[test]
    fn rejects_error_documents_and_html() {
        let error = br#"<?xml version="1.0"?><error code="100" description="Invalid API Key"/>"#;
        assert_eq!(items(error).unwrap_err(), "indexer error 100: Invalid API Key");
        assert_eq!(items(b"<html><body>Just a moment</body></html>").unwrap_err(), "expected an RSS feed, got <html>");
        assert_eq!(items(b"").unwrap_err(), "empty document");
    }
}
