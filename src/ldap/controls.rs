//! Hand-encoded LDAP request controls that ldap3 doesn't ship — currently the
//! Server-Side Sort control (RFC 2891), which census needs for **keyset paging**
//! over huge directories: a sorted, range-filtered page is the seek-shaped fetch a
//! [`mullion::RecordSource`] wants. Encoded with ldap3's re-exported `lber` (asn1)
//! writer, mirroring ldap3's own `PagedResults` control impl.

use bytes::BytesMut;
use ldap3::asn1::{write, ASNTag, Boolean, OctetString, Sequence, TagClass, Tag};
use ldap3::controls::RawControl;

/// Server-Side Sort request control OID (RFC 2891).
pub const SSS_OID: &str = "1.2.840.113556.1.4.473";
/// Server-Side Sort *response* control OID (matched off `res.ctrls` if needed).
#[allow(dead_code)] // Phase B: decode the SortResult for precise error reporting
pub const SSS_RESPONSE_OID: &str = "1.2.840.113556.1.4.474";
/// Virtual List View request control OID (RFC 2891) — Phase B (exact scrollbar).
pub const VLV_OID: &str = "2.16.840.1.113730.3.4.9";
/// Simple Paged Results control OID (RFC 2696) — advertised by nearly every server.
pub const PAGED_OID: &str = "1.2.840.113556.1.4.319";

/// `caseIgnoreOrderingMatch` — the ordering rule census names for string sort keys.
pub const CASE_IGNORE_ORDERING: &str = "2.5.13.3";

/// A Server-Side Sort request sorting by a single string attribute, ascending or
/// descending. The `orderingRule` is set **explicitly** to `caseIgnoreOrderingMatch`:
/// OpenLDAP refuses to sort by an attribute (like `uid`) whose schema declares no
/// ORDERING ("serverSort control: No ordering rule") unless one is named, and naming
/// it is harmless on servers (e.g. 389-ds) that would otherwise default it. Sent
/// non-critical — census only uses it once capability detection confirms SSS support.
///
/// `SortKeyList ::= SEQUENCE OF SEQUENCE { attributeType OCTET STRING,
///     orderingRule [0] OCTET STRING OPTIONAL, reverseOrder [1] BOOLEAN DEFAULT FALSE }`
pub fn sort_control(attr: &str, reverse: bool) -> RawControl {
    let mut key = vec![
        Tag::OctetString(OctetString { inner: attr.as_bytes().to_vec(), ..Default::default() }),
        // orderingRule [0] OCTET STRING — required by OpenLDAP for uid/cn.
        Tag::OctetString(OctetString {
            id: 0,
            class: TagClass::Context,
            inner: CASE_IGNORE_ORDERING.as_bytes().to_vec(),
        }),
    ];
    if reverse {
        // reverseOrder [1] BOOLEAN — context-tagged, only emitted when true (DEFAULT FALSE).
        key.push(Tag::Boolean(Boolean { id: 1, class: TagClass::Context, inner: true }));
    }
    let list = Tag::Sequence(Sequence {
        inner: vec![Tag::Sequence(Sequence { inner: key, ..Default::default() })],
        ..Default::default()
    })
    .into_structure();
    let mut buf = BytesMut::new();
    write::encode_into(&mut buf, list).expect("SSS control encodes");
    RawControl { ctype: SSS_OID.to_owned(), crit: false, val: Some(buf.to_vec()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascending_uid_is_byte_exact() {
        // SEQUENCE { SEQUENCE { OCTETSTRING "uid", [0] OCTETSTRING "2.5.13.3" } }
        let c = sort_control("uid", false);
        assert_eq!(c.ctype, SSS_OID);
        assert!(!c.crit);
        assert_eq!(
            c.val.unwrap(),
            vec![
                0x30, 0x11, 0x30, 0x0f, 0x04, 0x03, b'u', b'i', b'd',
                0x80, 0x08, 0x32, 0x2e, 0x35, 0x2e, 0x31, 0x33, 0x2e, 0x33, // [0] "2.5.13.3"
            ]
        );
    }

    #[test]
    fn descending_uid_appends_reverse_boolean() {
        // … same, plus [1] BOOLEAN true at the end.
        let val = sort_control("uid", true).val.unwrap();
        assert_eq!(
            &val[..21],
            &[
                0x30, 0x14, 0x30, 0x12, 0x04, 0x03, b'u', b'i', b'd',
                0x80, 0x08, 0x32, 0x2e, 0x35, 0x2e, 0x31, 0x33, 0x2e, 0x33,
                0x81, 0x01, // [1] BOOLEAN, len 1
            ]
        );
        assert_ne!(val[21], 0x00); // BER TRUE (0x01 or 0xFF)
    }
}
