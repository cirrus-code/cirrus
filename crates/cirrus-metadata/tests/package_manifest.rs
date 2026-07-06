//! End-to-end tests for [`PackageManifest`] — exercising both forms
//! (standalone `package.xml` and SOAP `unpackaged`) and confirming
//! quick-xml can round-trip the standalone output.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use cirrus_metadata::{MetadataType, PackageManifest};
use quick_xml::Reader;
use quick_xml::escape::unescape;
use quick_xml::events::Event;

#[test]
fn to_xml_is_well_formed_and_parses() {
    let pkg = PackageManifest::new("66.0")
        .add(MetadataType::APEX_CLASS, ["Foo", "Bar"])
        .add(MetadataType::CUSTOM_OBJECT, ["Account__c"])
        .all(MetadataType::CUSTOM_TAB);

    let xml = pkg.to_xml();

    // Sanity-check by walking the document with a real XML parser.
    // If the manifest emits malformed XML, read_event() will return
    // Err on the bad token.
    let mut reader = Reader::from_str(&xml);
    let mut depth: i32 = 0;
    let mut saw_package = false;
    let mut saw_version = false;
    let mut type_names = Vec::new();
    let mut members = Vec::new();
    let mut current_tag: Option<Vec<u8>> = None;

    loop {
        match reader.read_event().unwrap() {
            Event::Start(e) => {
                depth += 1;
                let local = e.name().local_name().as_ref().to_vec();
                if local == b"Package" {
                    saw_package = true;
                }
                current_tag = Some(local);
            }
            Event::End(_) => {
                depth -= 1;
                current_tag = None;
            }
            Event::Text(t) => {
                if let Some(tag) = &current_tag {
                    let text = unescape(&t.decode().unwrap()).unwrap().into_owned();
                    if tag == b"name" {
                        type_names.push(text);
                    } else if tag == b"members" {
                        members.push(text);
                    } else if tag == b"version" {
                        assert_eq!(text, "66.0");
                        saw_version = true;
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

    assert!(saw_package, "missing <Package> root");
    assert!(saw_version, "missing <version> element");
    assert_eq!(depth, 0, "unbalanced elements");
    assert_eq!(
        type_names,
        vec![
            "ApexClass".to_string(),
            "CustomObject".to_string(),
            "CustomTab".to_string()
        ]
    );
    assert!(members.contains(&"Foo".to_string()));
    assert!(members.contains(&"Bar".to_string()));
    assert!(members.contains(&"Account__c".to_string()));
    assert!(members.contains(&"*".to_string()));
}

#[test]
fn to_xml_handles_special_characters_safely() {
    // Member names with XML-reserved chars must be escaped — passing
    // the rendered output through quick-xml shouldn't fail on the
    // ampersand or angle brackets.
    let pkg = PackageManifest::new("66.0").add(MetadataType::APEX_CLASS, ["A&B", "C<D>E"]);
    let xml = pkg.to_xml();
    let mut reader = Reader::from_str(&xml);
    // Walk without panicking; that's the assertion.
    while reader.read_event().unwrap() != Event::Eof {}
    assert!(xml.contains("A&amp;B"));
    assert!(xml.contains("C&lt;D&gt;E"));
}

#[test]
fn empty_manifest_renders_minimal_valid_xml() {
    let pkg = PackageManifest::new("58.0");
    let xml = pkg.to_xml();
    let mut reader = Reader::from_str(&xml);
    let mut depth: i32 = 0;
    loop {
        match reader.read_event().unwrap() {
            Event::Start(_) => depth += 1,
            Event::End(_) => depth -= 1,
            Event::Eof => break,
            _ => {}
        }
    }
    assert_eq!(depth, 0);
    assert!(xml.contains("<version>58.0</version>"));
}

#[test]
fn packaged_manifest_emits_full_name_before_types() {
    let pkg = PackageManifest::new("66.0")
        .full_name("MyManagedPkg")
        .add(MetadataType::APEX_CLASS, ["Foo"]);

    let xml = pkg.to_xml();
    let mut reader = Reader::from_str(&xml);
    let mut element_order = Vec::new();
    loop {
        match reader.read_event().unwrap() {
            Event::Start(e) => {
                let local = e.name().local_name().as_ref().to_vec();
                element_order.push(local);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    // Find positions of <fullName>, <types>, <version> at top level.
    let i_full = element_order.iter().position(|t| t == b"fullName").unwrap();
    let i_types = element_order.iter().position(|t| t == b"types").unwrap();
    let i_version = element_order.iter().position(|t| t == b"version").unwrap();
    assert!(i_full < i_types);
    assert!(i_types < i_version);
}
