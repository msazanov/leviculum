//! Replies against the reference (`VEC-MSG-REPLY`, `VEC-MSG-COMMENT` in
//! `docs/src/appendix/lxmf/vectors/vectors.json`, written by
//! `gen_vectors.py` importing `reference/LXMF`).
//!
//! Both directions, as for reactions: the reference's message is read by
//! our codec with the target and the quote it holds, and the message built
//! from our fields is the reference's byte for byte.

mod common;

use leviculum_core::Identity;
use leviculum_lxmf::constants::{FIELD_COMMENT, FIELD_REPLY_QUOTE, FIELD_REPLY_TO};
use leviculum_lxmf::reply::{comment_field_value, Reply};
use leviculum_lxmf::{DeliveryMethod, Message, Verification};

fn hex(id: &str, field: &str) -> Vec<u8> {
    hex::decode(common::fixture(id, field)).unwrap()
}

fn target(id: &str) -> [u8; 32] {
    hex(id, "target_hex").try_into().unwrap()
}

fn expected(id: &str) -> Reply {
    Reply {
        to: target(id),
        quote: match id {
            "VEC-MSG-REPLY" => Some(String::from_utf8(hex(id, "recovered_quote_hex")).unwrap()),
            _ => None,
        },
    }
}

#[test]
fn the_references_reply_is_read_with_its_target() {
    for id in ["VEC-MSG-REPLY", "VEC-MSG-COMMENT"] {
        let packed = hex(id, "packed_hex");
        let identity = Identity::from_private_key_bytes(&hex(id, "src_identity_prv_hex")).unwrap();
        let message = Message::unpack(&packed, None, Some(&identity), DeliveryMethod::Direct)
            .unwrap_or_else(|e| panic!("{id}: {e}"));
        assert_eq!(message.verification, Verification::Valid, "{id}");
        assert_eq!(message.content, hex(id, "content_hex"), "{id}");
        assert_eq!(
            Reply::from_fields(&message.fields),
            Some(expected(id)),
            "{id}"
        );
        assert_eq!(
            hex(id, "recovered_reply_to_hex"),
            target(id).to_vec(),
            "{id}"
        );
    }
}

#[test]
fn our_reply_is_the_references_bytes() {
    for id in ["VEC-MSG-REPLY", "VEC-MSG-COMMENT"] {
        let identity = Identity::from_private_key_bytes(&hex(id, "src_identity_prv_hex")).unwrap();
        let packed = hex(id, "packed_hex");
        let fields = match id {
            "VEC-MSG-REPLY" => {
                let fields = expected(id).fields();
                assert_eq!(fields[0], (FIELD_REPLY_TO, hex(id, "field_30_msgpack_hex")));
                assert_eq!(
                    fields[1],
                    (FIELD_REPLY_QUOTE, hex(id, "field_31_msgpack_hex"))
                );
                fields
            }
            _ => {
                let value = comment_field_value(&target(id));
                assert_eq!(value, hex(id, "field_41_msgpack_hex"));
                vec![(FIELD_COMMENT, value)]
            }
        };
        let ours = Message::create(
            packed[0..16].try_into().unwrap(),
            packed[16..32].try_into().unwrap(),
            &identity,
            common::fixture(id, "timestamp").parse().unwrap(),
            Vec::new(),
            hex(id, "content_hex"),
            fields,
            DeliveryMethod::Opportunistic,
        )
        .unwrap();
        assert_eq!(ours.pack(), packed, "{id}: packed");
        assert_eq!(common::fixture(id, "signature_validated"), "true");
    }
}
