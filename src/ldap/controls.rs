//! Hand-encoded LDAP request controls that ldap3 doesn't ship — currently the
//! Server-Side Sort control (RFC 2891), which census needs for **keyset paging**
//! over huge directories: a sorted, range-filtered page is the seek-shaped fetch a
//! [`mullion::RecordSource`] wants. Encoded with ldap3's re-exported `lber` (asn1)
//! writer, mirroring ldap3's own `PagedResults` control impl.

use bytes::BytesMut;
use ldap3::asn1::{
    parse_tag, parse_uint, write, ASNTag, Boolean, Integer, OctetString, Sequence, StructureTag,
    TagClass, Tag, Types,
};
use ldap3::controls::RawControl;

/// Server-Side Sort request control OID (RFC 2891).
pub const SSS_OID: &str = "1.2.840.113556.1.4.473";
/// Server-Side Sort *response* control OID (matched off `res.ctrls` if needed).
#[allow(dead_code)] // Phase B: decode the SortResult for precise error reporting
pub const SSS_RESPONSE_OID: &str = "1.2.840.113556.1.4.474";
/// Virtual List View request control OID (RFC 2891).
pub const VLV_OID: &str = "2.16.840.1.113730.3.4.9";
/// Virtual List View *response* control OID (read off `res.ctrls`).
pub const VLV_RESPONSE_OID: &str = "2.16.840.1.113730.3.4.10";
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
    control(SSS_OID, list)
}

/// A Virtual List View request positioning a window **byValue**: `before` entries
/// before, and `after` entries after, the first entry whose sort key is `>= target`.
/// Pairs with an SSS control (which supplies the ordering) — this is how census pages
/// a huge list even when the sort attribute has no schema ORDERING rule (e.g. `uid`
/// on OpenLDAP), where a `(uid>=x)` range filter can't be evaluated. No `contextID`
/// (census fetches are stateless; the server re-derives the position each time).
///
/// `VirtualListViewRequest ::= SEQUENCE { beforeCount INTEGER, afterCount INTEGER,
///   target CHOICE { byoffset [0] SEQUENCE {offset,contentCount},
///                   greaterThanOrEqual [1] AssertionValue }, contextID OCTET STRING OPTIONAL }`
pub fn vlv_by_value(before: i32, after: i32, target: &str) -> RawControl {
    let seq = Tag::Sequence(Sequence {
        inner: vec![
            int(before),
            int(after),
            // greaterThanOrEqual [1] AssertionValue (context-primitive octet string).
            Tag::OctetString(OctetString { id: 1, class: TagClass::Context, inner: target.as_bytes().to_vec() }),
        ],
        ..Default::default()
    })
    .into_structure();
    control(VLV_OID, seq)
}

/// A Virtual List View request positioning a window **byOffset** (1-based `offset`
/// within `content_count` rows; pass `content_count = 0` when unknown, e.g. the first
/// fetch). Used to fetch the very start of the list.
pub fn vlv_by_offset(before: i32, after: i32, offset: i32, content_count: i32) -> RawControl {
    // target byoffset [0] SEQUENCE { offset INTEGER, contentCount INTEGER }
    let by_offset = Tag::Sequence(Sequence {
        id: 0,
        class: TagClass::Context,
        inner: vec![int(offset), int(content_count)],
    });
    let seq = Tag::Sequence(Sequence {
        inner: vec![int(before), int(after), by_offset],
        ..Default::default()
    })
    .into_structure();
    control(VLV_OID, seq)
}

/// Decode a VLV *response* control value → `(targetPosition, contentCount)` (both
/// 1-based / totals), for an exact scrollbar. `virtualListViewResult`/`contextID` are
/// ignored. Returns `None` on any parse failure.
///
/// `VirtualListViewResponse ::= SEQUENCE { targetPosition INTEGER, contentCount INTEGER,
///   virtualListViewResult ENUMERATED, contextID OCTET STRING OPTIONAL }`
pub fn parse_vlv_response(val: &[u8]) -> Option<(u64, u64)> {
    let tag = parse_tag(val).ok()?.1;
    let mut comps = tag.expect_constructed()?.into_iter();
    let mut next_uint = || -> Option<u64> {
        let bytes = comps
            .next()?
            .match_class(TagClass::Universal)
            .and_then(|t| t.match_id(Types::Integer as u64))
            .and_then(|t| t.expect_primitive())?;
        Some(parse_uint(bytes.as_slice()).ok()?.1)
    };
    let position = next_uint()?;
    let count = next_uint()?;
    Some((position, count))
}

/// A universal INTEGER tag.
fn int(v: i32) -> Tag {
    Tag::Integer(Integer { inner: v as i64, ..Default::default() })
}

/// Wrap an encoded structure into a non-critical [`RawControl`] for `oid`.
fn control(oid: &str, tag: StructureTag) -> RawControl {
    let mut buf = BytesMut::new();
    write::encode_into(&mut buf, tag).expect("control encodes");
    RawControl { ctype: oid.to_owned(), crit: false, val: Some(buf.to_vec()) }
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

    #[test]
    fn vlv_by_value_is_byte_exact() {
        // SEQUENCE { INTEGER 0, INTEGER 2, [1] "ab" }
        let c = vlv_by_value(0, 2, "ab");
        assert_eq!(c.ctype, VLV_OID);
        assert_eq!(
            c.val.unwrap(),
            vec![0x30, 0x0a, 0x02, 0x01, 0x00, 0x02, 0x01, 0x02, 0x81, 0x02, b'a', b'b']
        );
    }

    #[test]
    fn vlv_by_offset_is_byte_exact() {
        // SEQUENCE { INTEGER 0, INTEGER 2, [0] SEQUENCE { INTEGER 1, INTEGER 0 } }
        let c = vlv_by_offset(0, 2, 1, 0);
        assert_eq!(
            c.val.unwrap(),
            vec![
                0x30, 0x0e, 0x02, 0x01, 0x00, 0x02, 0x01, 0x02,
                0xa0, 0x06, 0x02, 0x01, 0x01, 0x02, 0x01, 0x00,
            ]
        );
    }

    #[test]
    fn parse_vlv_response_reads_position_and_count() {
        // SEQUENCE { INTEGER 5 (pos), INTEGER 100 (count), ENUMERATED 0 (result) }
        let bytes = [0x30, 0x09, 0x02, 0x01, 0x05, 0x02, 0x01, 0x64, 0x0a, 0x01, 0x00];
        assert_eq!(parse_vlv_response(&bytes), Some((5, 100)));
    }
}
