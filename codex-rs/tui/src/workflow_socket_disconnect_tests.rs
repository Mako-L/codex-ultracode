use super::*;

#[tokio::test]
async fn disconnect_closes_socket_even_while_reader_owns_bridge() {
    let (client, mut server) = std::os::unix::net::UnixStream::pair().unwrap();
    server
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let bridge = WorkflowBridge::from_socket(client).unwrap();
    bridge.disconnect("attachment failed");
    assert_eq!(server.read(&mut [0]).unwrap(), 0);
}
