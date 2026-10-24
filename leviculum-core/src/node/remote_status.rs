//! Firmware-neutral matching and sending for authenticated `/status` events.
//!
//! Authorization remains in the destination's request handler. Match the exact
//! management destination and path before building any status rows or response
//! bytes. Encoding and optional link-stat parsing use the shared status codec.

use rand_core::CryptoRngCore;

use super::{NodeCore, NodeEvent};
use crate::resource::ResourceError;
use crate::status_bundle::request_wants_link_stats;
use crate::traits::{Clock, Storage};
use crate::transport::TickOutput;
use crate::{DestinationHash, LinkId, RequestError};

/// The request path served, as Python registers it
/// (`Transport.py:253-259`).
pub const STATUS_PATH: &str = "/status";

/// Fixed-size context copied only from a matching management request.
#[derive(Clone, Copy, Debug)]
pub struct StatusRequest {
    pub link_id: LinkId,
    pub request_id: [u8; 16],
    pub include_link_stats: bool,
}

/// Match before allocating. A node with no management service cannot match.
pub fn status_request(
    event: &NodeEvent,
    management_destination: Option<&DestinationHash>,
) -> Option<StatusRequest> {
    let NodeEvent::RequestReceived {
        link_id,
        destination_hash,
        request_id,
        path,
        data,
        ..
    } = event
    else {
        return None;
    };
    if management_destination != Some(destination_hash) || path != STATUS_PATH {
        return None;
    }
    Some(StatusRequest {
        link_id: *link_id,
        request_id: *request_id,
        include_link_stats: request_wants_link_stats(data),
    })
}

/// Preserve which send failed; unrelated packet errors never start a Resource.
#[derive(Debug)]
pub enum StatusSendError {
    Packet(RequestError),
    Resource(ResourceError),
}

/// Try a packet first; only a payload-size refusal may use a response Resource.
///
/// No response copy is retained here. Resource ownership/allocation is the core
/// sender's normal response framing and encrypted transfer state.
pub fn send_status_response<R, C, S>(
    node: &mut NodeCore<R, C, S>,
    request: &StatusRequest,
    response: &[u8],
) -> Result<TickOutput, StatusSendError>
where
    R: CryptoRngCore,
    C: Clock,
    S: Storage,
{
    match node.send_response(&request.link_id, &request.request_id, response) {
        Ok(output) => Ok(output),
        Err(RequestError::PayloadTooLarge) => node
            .send_response_resource(&request.link_id, &request.request_id, response)
            .map(|(_, output)| output)
            .map_err(StatusSendError::Resource),
        Err(error) => Err(StatusSendError::Packet(error)),
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;
    use rand_core::OsRng;

    use super::*;
    use crate::identity::Identity;
    use crate::mgmt_allow_store::{remote_mgmt_decision, RemoteMgmtDecision, StoredMgmtAllow};
    use crate::node::{NodeCoreBuilder, RequestPolicy};
    use crate::packet::{PacketContext, PacketHeader};
    use crate::status_bundle::{encode_status_response, InterfaceStatus, StatusBundle};
    use crate::test_utils::{MockClock, TEST_TIME_MS};
    use crate::traits::NoStorage;
    use crate::transport::{Action, InterfaceId};
    use crate::{Destination, DestinationType, Direction};

    type Node = NodeCore<OsRng, MockClock, NoStorage>;

    fn packets(output: &TickOutput) -> Vec<Vec<u8>> {
        output
            .actions
            .iter()
            .map(|action| match action {
                Action::Broadcast { data, .. } | Action::SendPacket { data, .. } => data.clone(),
            })
            .collect()
    }

    fn one_packet(output: &TickOutput) -> Vec<u8> {
        let mut packets = packets(output);
        assert_eq!(packets.len(), 1);
        packets.remove(0)
    }

    fn board(stored: Option<&StoredMgmtAllow>) -> Node {
        let builder = NodeCoreBuilder::new();
        let builder = match remote_mgmt_decision(stored) {
            RemoteMgmtDecision::Disabled => builder,
            RemoteMgmtDecision::Enabled(allowed) => builder.remote_management(true, allowed),
        };
        builder.build(OsRng, MockClock::new(TEST_TIME_MS), NoStorage)
    }

    struct Pair {
        client: Node,
        server: Node,
        client_link: LinkId,
        server_link: LinkId,
    }

    impl Pair {
        fn connect(mut server: Node, destination: DestinationHash) -> Self {
            let signing_key = server
                .destination(&destination)
                .unwrap()
                .identity()
                .unwrap()
                .ed25519_verifying()
                .to_bytes();
            let mut client =
                NodeCoreBuilder::new().build(OsRng, MockClock::new(TEST_TIME_MS), NoStorage);
            let (client_link, _, output) = client.connect(destination, &signing_key).unwrap();
            let link_request = one_packet(&output);
            let server_link = crate::link::Link::calculate_link_id(&link_request);
            let proof = server.handle_packet(InterfaceId(0), &link_request);
            let rtt = client.handle_packet(InterfaceId(0), &one_packet(&proof));
            let established = server.handle_packet(InterfaceId(0), &one_packet(&rtt));
            assert!(established.events.iter().any(|event| matches!(
                event,
                NodeEvent::LinkEstablished { destination_hash, .. }
                    if *destination_hash == destination
            )));
            Self {
                client,
                server,
                client_link,
                server_link,
            }
        }

        fn allowed(identity: &Identity) -> Self {
            let stored = StoredMgmtAllow::from_hashes(&[*identity.hash()]).unwrap();
            let server = board(Some(&stored));
            let destination = *server.remote_mgmt_dest_hash().unwrap();
            let mut pair = Self::connect(server, destination);
            pair.identify(identity);
            pair
        }

        fn identify(&mut self, identity: &Identity) {
            let output = self
                .client
                .identify_link(&self.client_link, identity)
                .unwrap();
            let _ = self
                .server
                .handle_packet(InterfaceId(0), &one_packet(&output));
        }

        fn request(&mut self, path: &str, data: Option<&[u8]>) -> TickOutput {
            let (_, output) = self
                .client
                .send_request(&self.client_link, path, data, None)
                .unwrap();
            self.server
                .handle_packet(InterfaceId(0), &one_packet(&output))
        }

        fn status(&mut self) -> StatusRequest {
            let output = self.request("/status", Some(&[0x91, 0xc3]));
            output
                .events
                .iter()
                .find_map(|event| status_request(event, self.server.remote_mgmt_dest_hash()))
                .expect("authenticated allowed identity must reach /status")
        }

        fn receive_response(&mut self, output: TickOutput, request: &StatusRequest) -> Vec<u8> {
            let mut to_client = packets(&output);
            let mut response = None;
            for _ in 0..128 {
                if to_client.is_empty() {
                    break;
                }
                let mut to_server = Vec::new();
                for packet in core::mem::take(&mut to_client) {
                    let output = self.client.handle_packet(InterfaceId(0), &packet);
                    for event in &output.events {
                        if let NodeEvent::ResponseReceived {
                            request_id,
                            response_data,
                            ..
                        } = event
                        {
                            assert_eq!(*request_id, request.request_id);
                            assert!(response.is_none(), "response delivered twice");
                            response = Some(response_data.clone());
                        }
                    }
                    to_server.extend(packets(&output));
                }
                for packet in to_server {
                    to_client.extend(packets(&self.server.handle_packet(InterfaceId(0), &packet)));
                }
            }
            assert!(to_client.is_empty(), "response exchange must settle");
            response.expect("client must receive the correlated response")
        }
    }

    fn response(node: &Node, row_count: usize, include_links: bool) -> Vec<u8> {
        let rows: Vec<_> = (0..row_count)
            .map(|_| InterfaceStatus {
                name: "serial_usb",
                status: true,
                mode: crate::traits::InterfaceMode::Gateway.as_u8(),
                clients: None,
                rxb: 1234,
                txb: 5678,
                rxs: 12.0,
                txs: 56.0,
                bitrate: None,
            })
            .collect();
        encode_status_response(
            &StatusBundle {
                interfaces: &rows,
                transport_id: Some(node.identity().hash()),
                transport_uptime: 123.0,
            },
            include_links.then(|| node.transport_link_count() as u64),
        )
    }

    #[test]
    fn absent_and_empty_boot_acls_refuse_management_links() {
        for stored in [None, Some(&StoredMgmtAllow::EMPTY)] {
            let mut server = board(stored);
            // Address the actual management hash for this board identity,
            // not an arbitrary nonexistent destination.
            let identity =
                Identity::from_private_key_bytes(&server.identity().private_key_bytes().unwrap())
                    .unwrap();
            let signing_key = identity.ed25519_verifying().to_bytes();
            let destination = Destination::new(
                Some(identity),
                Direction::In,
                DestinationType::Single,
                "rnstransport",
                &["remote", "management"],
            )
            .unwrap();
            let mut client =
                NodeCoreBuilder::new().build(OsRng, MockClock::new(TEST_TIME_MS), NoStorage);
            let (_, _, output) = client.connect(*destination.hash(), &signing_key).unwrap();
            let rejected = server.handle_packet(InterfaceId(0), &one_packet(&output));
            assert!(
                rejected.actions.is_empty(),
                "disabled service must not prove a link"
            );
            assert!(!rejected.events.iter().any(|event| matches!(
                event,
                NodeEvent::LinkEstablished { .. } | NodeEvent::RequestReceived { .. }
            )));
        }
    }

    #[test]
    fn registered_empty_acl_rejects_an_authenticated_status_request() {
        let server = NodeCoreBuilder::new()
            .remote_management(true, Vec::new())
            .build(OsRng, MockClock::new(TEST_TIME_MS), NoStorage);
        let destination = *server.remote_mgmt_dest_hash().unwrap();
        let mut pair = Pair::connect(server, destination);
        pair.identify(&Identity::generate(&mut OsRng));
        let rejected = pair.request("/status", Some(&[0x91, 0xc3]));
        assert!(!rejected
            .events
            .iter()
            .any(|event| matches!(event, NodeEvent::RequestReceived { .. })));
        assert!(rejected.actions.is_empty());
    }

    #[test]
    fn wrong_and_unidentified_identities_never_reach_the_status_handler() {
        let allowed = Identity::generate(&mut OsRng);
        let stored = StoredMgmtAllow::from_hashes(&[*allowed.hash()]).unwrap();
        for identify in [false, true] {
            let server = board(Some(&stored));
            let destination = *server.remote_mgmt_dest_hash().unwrap();
            let mut pair = Pair::connect(server, destination);
            if identify {
                pair.identify(&Identity::generate(&mut OsRng));
            }
            let rejected = pair.request("/status", Some(&[0x91, 0xc3]));
            assert!(!rejected
                .events
                .iter()
                .any(|event| matches!(event, NodeEvent::RequestReceived { .. })));
            assert!(rejected.actions.is_empty());
        }
    }

    #[test]
    fn allowed_status_roundtrips_as_packet_and_response_resource() {
        for (row_count, context) in [
            (1, PacketContext::Response),
            (7, PacketContext::ResourceAdv),
        ] {
            let mut pair = Pair::allowed(&Identity::generate(&mut OsRng));
            let request = pair.status();
            assert!(request.include_link_stats);
            let encoded = response(&pair.server, row_count, request.include_link_stats);
            let output = send_status_response(&mut pair.server, &request, &encoded).unwrap();
            assert_eq!(
                PacketHeader::parse(&one_packet(&output)).unwrap().context,
                context
            );
            assert_eq!(pair.receive_response(output, &request), encoded);
        }
    }

    #[test]
    fn request_link_stats_uses_shared_strict_boolean_decoding() {
        let mut pair = Pair::allowed(&Identity::generate(&mut OsRng));
        for (data, expected) in [
            (None, false),
            (Some(&[0x90][..]), false),
            (Some(&[0x91, 0xc3][..]), true),
            (Some(&[0x91, 0xc2][..]), false),
            (Some(&[0x91, 0x01][..]), false),
            (Some(&[0xc3][..]), false),
        ] {
            let output = pair.request("/status", data);
            let request = output
                .events
                .iter()
                .find_map(|event| status_request(event, pair.server.remote_mgmt_dest_hash()))
                .unwrap();
            assert_eq!(request.include_link_stats, expected);
        }
    }

    #[test]
    fn authenticated_wrong_destination_and_path_produce_no_status_response() {
        let identity = Identity::generate(&mut OsRng);
        for wrong_destination in [false, true] {
            let stored = StoredMgmtAllow::from_hashes(&[*identity.hash()]).unwrap();
            let mut server = board(Some(&stored));
            let (destination, path) = if wrong_destination {
                let destination = Destination::new(
                    Some(Identity::generate(&mut OsRng)),
                    Direction::In,
                    DestinationType::Single,
                    "other",
                    &["status"],
                )
                .unwrap();
                let hash = *destination.hash();
                server.register_destination(destination);
                (hash, "/status")
            } else {
                (*server.remote_mgmt_dest_hash().unwrap(), "/other")
            };
            server.register_request_handler(
                destination,
                path,
                RequestPolicy::AllowList(vec![*identity.hash()]),
            );
            let mut pair = Pair::connect(server, destination);
            pair.identify(&identity);
            let received = pair.request(path, Some(&[0x91, 0xc3]));
            assert!(
                received
                    .events
                    .iter()
                    .any(|event| matches!(event, NodeEvent::RequestReceived { .. })),
                "the wrong endpoint's authorized handler must still run"
            );
            let mut outgoing = TickOutput::empty();
            for event in &received.events {
                if let Some(request) = status_request(event, pair.server.remote_mgmt_dest_hash()) {
                    let bytes = response(&pair.server, 1, request.include_link_stats);
                    outgoing
                        .merge(send_status_response(&mut pair.server, &request, &bytes).unwrap());
                }
                assert!(status_request(event, None).is_none());
            }
            assert!(outgoing.actions.is_empty());
            assert!(outgoing.events.is_empty());
        }
    }

    #[test]
    fn packet_errors_do_not_fall_back_and_resource_errors_remain_distinct() {
        let mut pair = Pair::allowed(&Identity::generate(&mut OsRng));
        let request = pair.status();
        let encoded = response(&pair.server, 7, true);
        let missing = StatusRequest {
            link_id: LinkId::new([0xa5; 16]),
            ..request
        };
        assert!(matches!(
            send_status_response(&mut pair.server, &missing, &encoded),
            Err(StatusSendError::Packet(RequestError::LinkNotFound))
        ));
        let _ = send_status_response(&mut pair.server, &request, &encoded).unwrap();
        assert!(pair
            .server
            .link(&pair.server_link)
            .unwrap()
            .has_outgoing_resource());
        assert!(matches!(
            send_status_response(&mut pair.server, &request, &encoded),
            Err(StatusSendError::Resource(ResourceError::TransferInProgress))
        ));
    }

    fn relay_count_tracks_storage<S: Storage>(storage: S) {
        let mut node = NodeCoreBuilder::new().build(OsRng, MockClock::new(TEST_TIME_MS), storage);
        let entry = crate::storage_types::LinkEntry {
            timestamp_ms: TEST_TIME_MS,
            next_hop_interface_index: 1,
            remaining_hops: 1,
            received_interface_index: 0,
            hops: 1,
            validated: false,
            proof_timeout_ms: TEST_TIME_MS + 10_000,
            destination_hash: [0x12; 16],
            peer_signing_key: None,
            received_peer: None,
            next_hop_peer: None,
        };
        node.storage_mut().set_link_entry([1; 16], entry.clone());
        node.storage_mut().set_link_entry([2; 16], entry.clone());
        assert_eq!(node.transport_link_count(), 2);
        node.storage_mut().set_link_entry(
            [1; 16],
            crate::storage_types::LinkEntry {
                validated: true,
                ..entry
            },
        );
        assert_eq!(
            node.transport_link_count(),
            2,
            "replacement must not increase count"
        );
        let removed = node.storage_mut().remove_link_entries_for_interface(1);
        assert_eq!(removed.len(), 2);
        assert_eq!(node.transport_link_count(), 0);
    }

    #[test]
    fn relay_count_tracks_insert_replace_and_removal_in_both_core_stores() {
        relay_count_tracks_storage(crate::embedded_storage::EmbeddedStorage::new());
        relay_count_tracks_storage(crate::memory_storage::MemoryStorage::compact());
    }

    #[test]
    fn endpoint_links_are_not_counted_as_transport_relays() {
        let pair = Pair::allowed(&Identity::generate(&mut OsRng));
        assert!(pair.server.link(&pair.server_link).unwrap().is_active());
        assert_eq!(pair.server.transport_link_count(), 0);
        assert_eq!(pair.client.transport_link_count(), 0);
    }
}
