#[test]
fn global_allocator_preserves_data_across_threaded_reallocation() {
    std::thread::scope(|scope| {
        for worker in 0u8..4 {
            scope.spawn(move || {
                for round in 0u8..8 {
                    let value = worker + round;
                    let mut bytes = vec![value; 256 * 1024];
                    bytes.resize(1024 * 1024, 0xa5);
                    assert!(bytes[..256 * 1024].iter().all(|&byte| byte == value));
                    assert!(bytes[256 * 1024..].iter().all(|&byte| byte == 0xa5));
                    bytes.truncate(128 * 1024);
                    bytes.shrink_to_fit();
                    assert!(bytes.iter().all(|&byte| byte == value));
                }
            });
        }
    });
}
