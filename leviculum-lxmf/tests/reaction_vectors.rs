//! `FIELD_REACTION` against the reference (`VEC-MSG-REACTION`,
//! `VEC-MSG-REACTION-TEXT` in `docs/src/appendix/lxmf/vectors/vectors.json`,
//! written by `gen_vectors.py` importing `reference/LXMF`).
//!
//! Both directions: the message the reference built is read by our codec
//! with the target and the content it holds, and the message our writer
//! builds from the same inputs is the reference's byte for byte, which the
//! vector shows the reference reading back with the right `REACTION_TO`.

mod common;

use leviculum_core::Identity;
use leviculum_lxmf::constants::FIELD_REACTION;
use leviculum_lxmf::reaction::Reaction;
use leviculum_lxmf::{DeliveryMethod, Message, Verification};

fn hex(id: &str, field: &str) -> Vec<u8> {
    hex::decode(common::fixture(id, field)).unwrap()
}

fn reaction_of(message: &Message) -> Option<Reaction> {
    message
        .fields
        .iter()
        .find(|(key, _)| *key == FIELD_REACTION)
        .and_then(|(_, value)| Reaction::decode_field_value(value))
}

fn expected(id: &str) -> Reaction {
    Reaction {
        to: hex(id, "reaction_to_hex").try_into().unwrap(),
        content: String::from_utf8(hex(id, "reaction_content_hex")).unwrap(),
    }
}

#[test]
fn the_references_reaction_is_read_with_its_target() {
    for id in ["VEC-MSG-REACTION", "VEC-MSG-REACTION-TEXT"] {
        let packed = hex(id, "packed_hex");
        let identity = Identity::from_private_key_bytes(&hex(id, "src_identity_prv_hex")).unwrap();
        let message = Message::unpack(&packed, None, Some(&identity), DeliveryMethod::Direct)
            .unwrap_or_else(|e| panic!("{id}: {e}"));
        assert_eq!(message.verification, Verification::Valid, "{id}");
        assert_eq!(message.message_id.to_vec(), hex(id, "message_id_hex"));
        assert_eq!(message.content, hex(id, "content_hex"), "{id}");
        let reaction = reaction_of(&message).unwrap_or_else(|| panic!("{id}: no reaction"));
        assert_eq!(reaction, expected(id), "{id}");
        assert_eq!(reaction.content, "\u{1F44D}");
    }
}

#[test]
fn our_reaction_is_the_references_bytes() {
    let identity =
        Identity::from_private_key_bytes(&hex("VEC-MSG-REACTION", "src_identity_prv_hex")).unwrap();
    for id in ["VEC-MSG-REACTION", "VEC-MSG-REACTION-TEXT"] {
        let packed = hex(id, "packed_hex");
        let value = expected(id).encode_field_value();
        assert_eq!(value, hex(id, "field_msgpack_hex"), "{id}: field value");
        let ours = Message::create(
            packed[0..16].try_into().unwrap(),
            packed[16..32].try_into().unwrap(),
            &identity,
            common::fixture(id, "timestamp").parse().unwrap(),
            Vec::new(),
            hex(id, "content_hex"),
            vec![(FIELD_REACTION, value)],
            DeliveryMethod::Opportunistic,
        )
        .unwrap();
        assert_eq!(ours.pack(), packed, "{id}: packed");
        // What the reference read back out of these bytes.
        assert_eq!(
            hex(id, "recovered_reaction_to_hex"),
            expected(id).to.to_vec(),
            "{id}"
        );
        assert_eq!(common::fixture(id, "signature_validated"), "true");
    }
}
