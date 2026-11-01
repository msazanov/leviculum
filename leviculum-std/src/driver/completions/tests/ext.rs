use super::*;

#[tokio::test]
async fn delivery_wakes_only_its_link_and_node_without_losing_early_wake() {
    let registry = CompletionRegistry::new();
    let other_node = CompletionRegistry::new();
    let a = registry.send_window(link(1));
    let b = registry.send_window(link(2));
    let c = other_node.send_window(link(1));
    let second_handle = registry.send_window(link(1));
    assert!(Arc::ptr_eq(&a, &second_handle));
    registry.release_send_window(link(1), &second_handle);
    let wa = a.notified();
    let wb = b.notified();
    let wc = c.notified();
    tokio::pin!(wa, wb, wc);
    wa.as_mut().enable();
    wb.as_mut().enable();
    wc.as_mut().enable();
    registry.observe(&NodeEvent::LinkDeliveryConfirmed {
        link_id: link(1),
        packet_hash: [0; 32],
    });
    tokio::time::timeout(std::time::Duration::from_millis(50), &mut wa)
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(5), &mut wb)
            .await
            .is_err()
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(5), &mut wc)
            .await
            .is_err()
    );
    registry.close();
    tokio::time::timeout(std::time::Duration::from_millis(50), &mut wb)
        .await
        .unwrap();
    registry.release_send_window(link(2), &b);
    assert!(!registry.send_windows.contains(&link(2)));
}

#[test]
fn last_handle_unregisters_even_when_a_wake_temporarily_owns_the_notify() {
    let registry = CompletionRegistry::new();
    let handle = registry.send_window(link(1));
    let concurrent_wake = Arc::clone(&handle);
    registry.release_send_window(link(1), &handle);
    assert!(registry.send_windows.is_empty());
    drop(concurrent_wake);
}
