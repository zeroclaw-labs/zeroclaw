//! Bounded legacy device lists. Unknown extensions fail closed rather than being lost.
use anyhow::{Context, Result, ensure};
use roxmltree::{Document, ParsingOptions};
use std::collections::BTreeMap;
const NS: &str = "eu.siacs.conversations.axolotl";
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceList(BTreeMap<u32, Option<String>>);
fn valid(id: u32) -> Result<()> {
    ensure!((1..=i32::MAX as u32).contains(&id), "Invalid device ID");
    Ok(())
}
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\'', "&apos;")
        .replace('"', "&quot;")
}
impl DeviceList {
    pub fn parse(xml: &str) -> Result<Self> {
        ensure!(xml.len() <= 128 * 1024, "Device list too large");
        let doc = Document::parse_with_options(
            xml,
            ParsingOptions {
                allow_dtd: false,
                nodes_limit: 2048,
            },
        )?;
        let root = doc.root_element();
        ensure!(
            root.has_tag_name((NS, "list")),
            "Unsupported device-list dialect"
        );
        ensure!(root.attributes().len() == 0, "Unknown list attributes");
        let mut devices = BTreeMap::new();
        for node in root.children().filter(|n| n.is_element()) {
            ensure!(
                node.has_tag_name((NS, "device")) && devices.len() < 256,
                "Unexpected device-list entry"
            );
            ensure!(
                !node.children().any(|n| n.is_element()
                    || n.is_text() && !n.text().unwrap_or_default().trim().is_empty()),
                "Nested device data"
            );
            ensure!(
                node.attributes()
                    .all(|a| a.namespace().is_none() && matches!(a.name(), "id" | "label")),
                "Unknown device attributes"
            );
            let raw = node.attribute("id").context("Missing device ID")?;
            ensure!(
                !raw.is_empty() && raw.bytes().all(|b| b.is_ascii_digit()),
                "Invalid device ID"
            );
            let id = raw.parse()?;
            valid(id)?;
            let label = node.attribute("label").map(str::to_owned);
            ensure!(
                label.as_ref().is_none_or(|s| s.chars().count() <= 128),
                "Device label too large"
            );
            ensure!(devices.insert(id, label).is_none(), "Duplicate device ID");
        }
        Ok(Self(devices))
    }
    /// Preserve all known devices and labels; never replace an existing own label.
    pub fn include_own(&mut self, id: u32) -> Result<bool> {
        valid(id)?;
        if self.0.contains_key(&id) {
            return Ok(false);
        }
        ensure!(self.0.len() < 256, "Device list is full");
        self.0.insert(id, None);
        Ok(true)
    }
    pub fn contains(&self, id: u32) -> bool {
        self.0.contains_key(&id)
    }

    pub fn xml(&self) -> String {
        let mut out = format!("<list xmlns='{NS}'>");
        for (id, label) in &self.0 {
            out.push_str(&format!("<device id='{id}'"));
            if let Some(label) = label {
                out.push_str(&format!(" label='{}'", escape(label)));
            }
            out.push_str("/>");
        }
        out.push_str("</list>");
        out
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_other_devices_labels_and_existing_own_label() {
        let mut list=DeviceList::parse(&format!("<list xmlns='{NS}'><device id='303' label='Telefone &amp; tablet'/><device id='202' label='Nome existente'/></list>")).unwrap();
        let before = list.clone();
        assert!(!list.include_own(202).unwrap());
        assert_eq!(list, before);
        assert!(list.include_own(404).unwrap());
        assert!(list.contains(303));
        assert_eq!(DeviceList::parse(&list.xml()).unwrap(), list);
        assert!(list.xml().contains("Telefone &amp; tablet"));
        assert!(list.xml().contains("Nome existente"));
    }
    #[test]
    fn rejects_duplicates_foreign_dialects_and_unknown_extensions() {
        for body in [
            "<device id='0'/>",
            "<device id='2'/><device id='2'/>",
            "<device id='2' custom='preserve-me'/>",
            "<device id='2'><data/></device>",
        ] {
            assert!(DeviceList::parse(&format!("<list xmlns='{NS}'>{body}</list>")).is_err());
        }
        assert!(DeviceList::parse("<devices xmlns='urn:xmpp:omemo:2'/>").is_err());
        let mut list = DeviceList::parse(&format!("<list xmlns='{NS}'/>")).unwrap();
        assert!(list.include_own(0).is_err());
        for id in 1..=256 {
            list.include_own(id).unwrap();
        }
        assert!(list.include_own(257).is_err());
    }
}
